//! TOML + environment configuration.
//!
//! Load order is layered so users can keep factory defaults intact:
//!
//! 1. `config/default.toml` — factory defaults, checked in, READ-ONLY by convention.
//! 2. `config/user.toml` — optional overlay; written by [`Settings::save_user`] whenever the
//!    user hits "Save settings" in the UI. Missing file is fine.
//! 3. `JARVIS__*` environment variables (separator `__`).
//!
//! "Restore defaults" = delete `config/user.toml` and re-run [`Settings::load`].

use bevy::ecs::resource::Resource;
use bevy::math::Quat;
use bevy::render::view::Msaa;
use bevy::window::PresentMode;
use config::{Config, Environment, File};
use serde::{Deserialize, Serialize};

/// Where we persist user overrides. Relative to the working directory at launch.
pub const USER_CONFIG_PATH: &str = "config/user.toml";
/// Factory defaults. Loaded first, then overlaid by [`USER_CONFIG_PATH`].
pub const DEFAULT_CONFIG_STEM: &str = "config/default";
pub const USER_CONFIG_STEM: &str = "config/user";

#[derive(Debug, Clone, Deserialize, Serialize, Resource)]
pub struct Settings {
    pub ironclaw: IronclawSettings,
    pub gateway: GatewaySettings,
    /// ZeroClaw gateway settings — alternate chat backend. Only one of
    /// `[gateway]` (IronClaw) and `[zeroclaw]` drives the chat UI at a time;
    /// the active one is chosen by `gateway.backend` (`"ironclaw"` (default)
    /// or `"zeroclaw"`).
    #[serde(default)]
    pub zeroclaw: ZeroClawSettings,
    pub tts: TtsSettings,
    pub avatar: AvatarSettings,
    pub camera: CameraSettings,
    pub graphics: GraphicsSettings,
    pub look_at: LookAtSettings,
    pub mcp: McpSettings,
    pub a2f: A2fSettings,
    pub kimodo: KimodoSettings,
    pub pose_library: PoseLibrarySettings,
    /// Home Assistant URL/token, device enable lists, and presence defaults.
    #[serde(default)]
    pub home_assistant: HomeAssistantSettings,
    /// Which debug UI windows are open, menu-bar preferences, etc. Persisted so the
    /// application reopens in the same layout.
    #[serde(default)]
    pub ui: UiSettings,
    /// UI-visible defaults for the Pose Controller.
    #[serde(default)]
    pub pose_controller: PoseControllerSettings,
    /// Key / fill / rim DirectionalLight rig driven by the Graphics Advanced window.
    #[serde(default)]
    pub light_rig: LightRigSettings,
    /// Path to the MToon per-material overrides JSON sidecar (auto-loaded on boot).
    #[serde(default)]
    pub mtoon_overrides: MToonOverridesSettings,
    /// Where emotion → (animation, expression, …) mappings live on disk.
    /// Defaults to `config/emotions.json`; see [`crate::emotions`].
    #[serde(default)]
    pub emotions: EmotionsSettings,
    /// Where animation-layer-set snapshots live on disk. Defaults to
    /// `config/anim_layer_sets.json`.
    #[serde(default)]
    pub anim_layer_sets: AnimLayerSetsSettings,
    /// Defaults for the in-engine animation layer stack (breathing / blink / fidgets).
    /// See `[anim_layers]` in `config/default.toml`.
    #[serde(default)]
    pub anim_layers: AnimLayersSettings,
}

/// Persistable debug-UI state: which dedicated windows are open. Everything else
/// (in-progress chat input, transient status strings, modal flags) stays on the
/// non-serialized `DebugUiState` resource.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UiSettings {
    #[serde(default = "default_true")]
    pub show_chat: bool,
    #[serde(default)]
    pub show_avatar: bool,
    #[serde(default)]
    pub show_camera: bool,
    #[serde(default)]
    pub show_live_test: bool,
    #[serde(default)]
    pub show_pose_controller: bool,
    /// Default side panel for any Pose Controller tab without a per-tab
    /// override in [`Self::pose_controller_tab_dock_sides`]. Allowed values:
    /// `"left"`, `"right"`, `"bottom"`, `"floating"`, or `"hidden"`. The
    /// default is `"right"` — the pre-Phase-4 layout.
    #[serde(default = "default_pose_dock_side")]
    pub pose_controller_dock_side: String,
    /// Saved width (px) of any docked Pose Controller side panel. Persisted
    /// so users don't have to re-resize on every launch. Applies to whichever
    /// side panel is rendered (left or right).
    #[serde(default = "default_pose_dock_width")]
    pub pose_controller_dock_width: f32,
    /// Saved height (px) of the docked bottom panel for any Pose Controller
    /// tab assigned to `"bottom"`.
    #[serde(default = "default_pose_dock_bottom_height")]
    pub pose_controller_dock_bottom_height: f32,
    /// Whitelist of tab ids the user has popped out into their own floating
    /// windows. Kept for backward compatibility — the new per-tab dock sides
    /// map below replaces this when present. Each id matches
    /// `PoseControllerTab::config_key()`.
    #[serde(default)]
    pub pose_controller_undocked_tabs: Vec<String>,
    /// Per-tab dock side override. Keys are `PoseControllerTab::config_key()`
    /// values; values are one of `"left"`, `"right"`, `"bottom"`,
    /// `"floating"`, or `"hidden"`. Tabs not in this map fall back to
    /// [`Self::pose_controller_dock_side`].
    #[serde(default)]
    pub pose_controller_tab_dock_sides: std::collections::HashMap<String, String>,
    /// Where the global Pose Tools toolbar (edit-mode toggle, axis selector,
    /// mirror controls, panel show/hide buttons) renders. Currently always
    /// `"top"` — kept as a string for forward compatibility with a `"none"`
    /// option that hides the toolbar entirely.
    #[serde(default = "default_pose_tools_toolbar_pos")]
    pub pose_tools_toolbar_pos: String,
    /// Viewport bone pick, euler gizmo helpers, and VRMC spring joint tuning.
    #[serde(default)]
    pub show_rig_editor: bool,
    #[serde(default)]
    pub show_graphics_advanced: bool,
    /// Dedicated "Animation Layers" window — timeline view of every active
    /// layer with per-layer enable / weight / play controls.
    #[serde(default)]
    pub show_anim_layers: bool,
    /// Where Animation Layers renders: `"bottom"` (dopesheet-style bottom
    /// panel — the default), `"floating"` (legacy `egui::Window`), or
    /// `"left"` / `"right"` for side docking.
    #[serde(default = "default_anim_layers_dock_side")]
    pub anim_layers_dock_side: String,
    /// Saved height (px) of the bottom Animation Layers panel.
    #[serde(default = "default_anim_layers_bottom_height")]
    pub anim_layers_bottom_height: f32,
    /// Emotion Mappings editor — bind `[ACT emotion="x"]` labels to VRM
    /// expressions / animations.
    #[serde(default)]
    pub show_emotion_mappings: bool,
    /// Home Assistant connection, device registry, and presence routing.
    #[serde(default)]
    pub show_home_assistant: bool,
    /// Raw traffic log (WS / SSE / HTTP) per external service.
    #[serde(default)]
    pub show_network_trace: bool,
    /// Phase-5 consolidation: unified Service Hub workspace combining
    /// Channel hub / Gateway / TTS / MCP / Services rows in tabs.
    #[serde(default)]
    pub show_service_hub: bool,
    /// Phase-5 consolidation: unified Graphics workspace combining basic
    /// lights, advanced post-process / MToon, and look-at controls.
    #[serde(default)]
    pub show_graphics_workspace: bool,
    /// Phase-5 consolidation: unified Diagnostics workspace combining
    /// avatar Y-diagnostics summary, network trace, and pipeline status.
    #[serde(default)]
    pub show_diagnostics_workspace: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            show_chat: true,
            show_avatar: false,
            show_camera: false,
            show_live_test: false,
            show_pose_controller: false,
            pose_controller_dock_side: default_pose_dock_side(),
            pose_controller_dock_width: default_pose_dock_width(),
            pose_controller_dock_bottom_height: default_pose_dock_bottom_height(),
            pose_controller_undocked_tabs: Vec::new(),
            pose_controller_tab_dock_sides: std::collections::HashMap::new(),
            pose_tools_toolbar_pos: default_pose_tools_toolbar_pos(),
            show_rig_editor: false,
            show_graphics_advanced: false,
            show_anim_layers: false,
            anim_layers_dock_side: default_anim_layers_dock_side(),
            anim_layers_bottom_height: default_anim_layers_bottom_height(),
            show_emotion_mappings: false,
            show_home_assistant: false,
            show_network_trace: false,
            show_service_hub: true,
            show_graphics_workspace: false,
            show_diagnostics_workspace: false,
        }
    }
}

/// Home Assistant REST + optional ha-voice-bridge proxy (same headers as Airi).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HomeAssistantSettings {
    /// e.g. `http://192.168.4.7:8123` — no trailing slash.
    #[serde(default)]
    pub ha_url: String,
    /// Long-lived access token.
    #[serde(default)]
    pub ha_token: String,
    /// If set, REST goes through `{bridge_url}/ha-proxy/...` with `X-HA-URL` / `X-HA-Token`.
    /// Empty = direct `ha_url` + `Authorization: Bearer`.
    #[serde(default)]
    pub bridge_url: String,
    #[serde(default)]
    pub default_area: String,
    #[serde(default = "default_presence_timeout_ms")]
    pub presence_timeout_ms: u64,
    #[serde(default)]
    pub enabled_camera_ids: Vec<String>,
    #[serde(default)]
    pub enabled_mic_ids: Vec<String>,
    #[serde(default)]
    pub enabled_speaker_ids: Vec<String>,
    #[serde(default)]
    pub detection_sensor_ids: Vec<String>,
    /// Poll HA object-detection sensors and drive VRM look-at (same idea as ha-voice-bridge).
    #[serde(default)]
    pub vision_gaze_enabled: bool,
    #[serde(default = "default_vision_gaze_poll_ms")]
    pub vision_gaze_poll_ms: u64,
    #[serde(default = "default_vision_gaze_depth")]
    pub vision_gaze_depth: f32,
    /// Normalized bbox coordinates from HA are assumed to be in this frame size (see `CAMERA_RES_*` in ha-voice-bridge).
    #[serde(default = "default_vision_gaze_image_w")]
    pub vision_gaze_image_width: f32,
    #[serde(default = "default_vision_gaze_image_h")]
    pub vision_gaze_image_height: f32,
    /// Negate horizontal look-at offset (camera / rig convention vs ha-voice-bridge default).
    #[serde(default)]
    pub vision_gaze_flip_horizontal: bool,
    /// ~Time constant (seconds) for exponential smoothing of look-at (no more raw snaps each poll).
    #[serde(default = "default_vision_gaze_smooth_tau_sec")]
    pub vision_gaze_smooth_tau_sec: f32,
    /// Scales the **horizontal** (X) part of the VRM-local look target. VRM eye `LookAt` range-maps
    /// yaw; if the implied yaw (degrees) exceeds the model’s `input_max`, the eyes sit at full
    /// left/right with almost no in-between. Lower this (e.g. 0.1–0.25) to keep motion in range.
    #[serde(default = "default_vision_gaze_horizontal_sensitivity")]
    pub vision_gaze_horizontal_sensitivity: f32,
    /// Optional 3-point horizontal map: `nx` at left / center / right in frame. When all set, map these to t∈[−1,0,1] and drive X (centre = look straight in X).
    #[serde(default)]
    pub vision_gaze_cal_nx_left: Option<f32>,
    #[serde(default)]
    pub vision_gaze_cal_nx_center: Option<f32>,
    #[serde(default)]
    pub vision_gaze_cal_nx_right: Option<f32>,
    /// Added to the computed VRM-local look target (m) after mapping — nudge "straight" or fix rig bias.
    #[serde(default)]
    pub vision_gaze_offset_x: f32,
    #[serde(default)]
    pub vision_gaze_offset_y: f32,
    #[serde(default)]
    pub vision_gaze_offset_z: f32,
}

impl Default for HomeAssistantSettings {
    fn default() -> Self {
        Self {
            ha_url: String::new(),
            ha_token: String::new(),
            bridge_url: String::new(),
            default_area: String::new(),
            presence_timeout_ms: default_presence_timeout_ms(),
            enabled_camera_ids: Vec::new(),
            enabled_mic_ids: Vec::new(),
            enabled_speaker_ids: Vec::new(),
            detection_sensor_ids: Vec::new(),
            vision_gaze_enabled: false,
            vision_gaze_poll_ms: default_vision_gaze_poll_ms(),
            vision_gaze_depth: default_vision_gaze_depth(),
            vision_gaze_image_width: default_vision_gaze_image_w(),
            vision_gaze_image_height: default_vision_gaze_image_h(),
            vision_gaze_flip_horizontal: false,
            vision_gaze_smooth_tau_sec: default_vision_gaze_smooth_tau_sec(),
            vision_gaze_horizontal_sensitivity: default_vision_gaze_horizontal_sensitivity(),
            vision_gaze_cal_nx_left: None,
            vision_gaze_cal_nx_center: None,
            vision_gaze_cal_nx_right: None,
            vision_gaze_offset_x: 0.0,
            vision_gaze_offset_y: 0.0,
            vision_gaze_offset_z: 0.0,
        }
    }
}

fn default_presence_timeout_ms() -> u64 {
    60_000
}

fn default_vision_gaze_poll_ms() -> u64 {
    150
}

fn default_vision_gaze_depth() -> f32 {
    2.0
}

fn default_vision_gaze_image_w() -> f32 {
    640.0
}

fn default_vision_gaze_image_h() -> f32 {
    480.0
}

fn default_vision_gaze_smooth_tau_sec() -> f32 {
    0.18
}

fn default_vision_gaze_horizontal_sensitivity() -> f32 {
    0.22
}

/// Where [`crate::emotions::EmotionMap`] persists its JSON sidecar.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmotionsSettings {
    #[serde(default = "default_emotions_path")]
    pub path: String,
}

impl Default for EmotionsSettings {
    fn default() -> Self {
        Self {
            path: default_emotions_path(),
        }
    }
}

fn default_emotions_path() -> String {
    crate::emotions::DEFAULT_EMOTIONS_PATH.to_string()
}

/// Where the animation-layers window persists named layer-set snapshots.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnimLayerSetsSettings {
    #[serde(default = "default_anim_layer_sets_path")]
    pub path: String,
}

impl Default for AnimLayerSetsSettings {
    fn default() -> Self {
        Self {
            path: default_anim_layer_sets_path(),
        }
    }
}

fn default_anim_layer_sets_path() -> String {
    "config/anim_layer_sets.json".to_string()
}

/// Boot-time behaviour for the binary crate’s animation layer stack (`anim_layers` plugin).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnimLayersSettings {
    /// When true and the stack has no layers yet, install breathing + blink +
    /// weight-shift + finger/toe fidgets and apply `master_enabled_default`.
    #[serde(default = "default_true")]
    pub auto_install_procedural: bool,
    /// Initial value for `LayerStack.master_enabled` after auto-install (or when
    /// the stack is still empty has no effect until layers exist).
    #[serde(default = "default_true")]
    pub master_enabled_default: bool,
    /// Optional named layer set loaded on boot (after auto-install). Empty = skip.
    #[serde(default)]
    pub boot_layer_set: String,
}

impl Default for AnimLayersSettings {
    fn default() -> Self {
        Self {
            auto_install_procedural: true,
            master_enabled_default: true,
            boot_layer_set: String::new(),
        }
    }
}

/// RMCP (Model Context Protocol) streamable-HTTP server that exposes
/// pose / A2F / Kimodo tools to IronClaw (and any other MCP client).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Socket the MCP HTTP transport binds to.
    pub bind_address: String,
    /// Path the RMCP streamable-HTTP service is nested at (e.g. `/mcp`).
    #[serde(default = "default_mcp_path")]
    pub path: String,
    /// Optional bearer token. If non-empty, requests must include
    /// `Authorization: Bearer <token>`.
    #[serde(default)]
    pub auth_token: String,
    /// RMCP closes the streamable-HTTP session worker after this many seconds
    /// with no session activity (default in rmcp 1.5 is 300). Cursor then logs
    /// "resume failed" until it opens a new session. Use `0` to disable the
    /// timeout (sessions only end when the client disconnects). Default:
    /// 86400 (24 hours).
    #[serde(default = "default_mcp_session_keep_alive_sec")]
    pub session_keep_alive_sec: u64,
}

fn default_mcp_path() -> String {
    "/mcp".to_string()
}

fn default_mcp_session_keep_alive_sec() -> u64 {
    86400
}

/// NVIDIA Audio2Face-3D Docker endpoint used by the A2F MCP tools.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct A2fSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// gRPC endpoint, e.g. `localhost:52000` or `http://192.168.4.8:52000`.
    pub endpoint: String,
    /// HTTP health probe, e.g. `http://localhost:8000/v1/health/ready`.
    pub health_url: String,
    /// Must match the Audio2Face NIM / service **`--function-id`** (avatar model).
    /// Example (Claire): `0961a6da-fb9e-4f2e-8491-247e5fd7bf8d`. Not sent on the gRPC wire;
    /// documented here so config, MCP `a2f_status`, and the running container stay aligned.
    #[serde(default)]
    pub function_id: String,
    /// After Kokoro returns playable audio, run `ProcessAudioStream` and drive
    /// [`PoseCommand::AnimateExpressions`] lip-sync on the VRM (same mapping as MCP tests).
    #[serde(default = "default_true")]
    pub apply_from_tts: bool,
}

/// Kimodo motion-generation timeouts / defaults. The service itself runs as a
/// separate process (Python) that connects *to* our hub; no URL is configured.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KimodoSettings {
    /// Seconds to wait for a `kimodo:status` reply before giving up.
    #[serde(default = "default_kimodo_timeout")]
    pub generate_timeout_sec: u64,
    /// Default `duration` when a caller omits it.
    #[serde(default = "default_kimodo_duration")]
    pub default_duration_sec: f32,
    /// Default denoising steps.
    #[serde(default = "default_kimodo_steps")]
    pub default_steps: u32,
}

fn default_kimodo_timeout() -> u64 {
    180
}
fn default_kimodo_duration() -> f32 {
    3.0
}
fn default_kimodo_steps() -> u32 {
    100
}

/// Where the filesystem-backed pose / animation library lives on disk. Defaults
/// follow the Node `pose-controller`'s paths so the two can coexist.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoseLibrarySettings {
    pub poses_dir: String,
    pub animations_dir: String,
}

/// Channel-server (IronClaw-protocol hub) the avatar HOSTS. `server.mjs`,
/// `ironclaw-proxy`, etc. connect to `ws://<bind_address>/ws`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IronclawSettings {
    /// Socket the axum hub binds to (WS `/ws` + HTTP `/broadcast` + `/health`).
    pub bind_address: String,
    /// Optional shared-secret. If non-empty, peers must send a matching
    /// `module:authenticate { token }` frame before they can publish/receive.
    #[serde(default, alias = "token")]
    pub auth_token: String,
    /// Identity used for envelopes the avatar itself publishes.
    pub module_name: String,
}

/// IronClaw gateway (port 3000 by default) — the rich chat surface used by the avatar.
/// Bearer-auth via `GATEWAY_AUTH_TOKEN` on the IronClaw side.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewaySettings {
    /// e.g. `http://192.168.4.8:3000` — no trailing slash.
    pub base_url: String,
    /// Static bearer token. Populated from `IRONCLAW_GATEWAY_TOKEN` env at startup.
    #[serde(default)]
    pub auth_token: String,
    /// Thread to auto-select on boot. Empty string = "no preference"; a fresh
    /// thread named `jarvis-avatar` is created on first run and its id persisted
    /// back here via `save_to_default()`.
    #[serde(default)]
    pub default_thread_id: String,
    /// Per-request timeout (ms) for non-streaming HTTP calls.
    #[serde(default = "default_gateway_timeout")]
    pub request_timeout_ms: u64,
    /// Max history turns loaded when switching threads.
    #[serde(default = "default_history_limit")]
    pub history_limit: u32,
    /// Which chat backend the chat UI talks to. `"ironclaw"` (default) keeps
    /// the historical IronClaw gateway path. `"zeroclaw"` activates the
    /// ZeroClaw plugins instead (see `[zeroclaw]`). Only one is active at a
    /// time — changing this needs an app restart.
    #[serde(default = "default_chat_backend")]
    pub backend: String,
}

fn default_chat_backend() -> String {
    "ironclaw".to_string()
}

/// Which chat backend [`Settings::gateway.backend`] selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatBackend {
    Ironclaw,
    Zeroclaw,
}

impl ChatBackend {
    /// Case-insensitive parse with `Ironclaw` as the safe default for unknown
    /// values.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "zeroclaw" | "zc" => ChatBackend::Zeroclaw,
            _ => ChatBackend::Ironclaw,
        }
    }
}

/// ZeroClaw gateway — alternate chat backend speaking the ZeroClaw REST + WS
/// surface (`/webhook`, `/ws/chat`, `/api/events`, `/api/memory`). Activated by
/// `gateway.backend = "zeroclaw"`; otherwise these settings are read but no
/// network traffic is initiated.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ZeroClawSettings {
    /// e.g. `http://192.168.4.8:42617` or `https://claw.shadowbroker.app` —
    /// no trailing slash.
    #[serde(default = "default_zeroclaw_base_url")]
    pub base_url: String,
    /// Optional WS base override (e.g. `wss://claw.shadowbroker.app`). Empty
    /// = derive from [`Self::base_url`] (`http` → `ws`, `https` → `wss`).
    #[serde(default)]
    pub ws_url: String,
    /// Bearer token sent on every `/api/*` request and as the `?token=` query
    /// param on `/ws/chat`. Populated from `JARVIS__ZEROCLAW__AUTH_TOKEN` or
    /// `ZEROCLAW_GATEWAY_TOKEN` env at startup. Leave empty when the gateway
    /// runs with `require_pairing = false`.
    #[serde(default)]
    pub auth_token: String,
    /// Optional `X-Webhook-Secret` value, sent on `POST /webhook` calls so
    /// ZeroClaw can attribute traffic to the avatar specifically.
    #[serde(default)]
    pub webhook_secret: String,
    /// Identification string sent as `X-Client` header and as the
    /// `User-Agent`. Default: `jarvis-avatar`.
    #[serde(default = "default_zeroclaw_client_id")]
    pub client_id: String,
    /// ZeroClaw agent alias — must match a `[agents.<alias>]` block on the
    /// gateway. **Required** for `/ws/chat` (the gateway returns 400 without
    /// it); also passed to `/webhook` so we don't depend on its silent
    /// "auto-pick the first enabled agent" behaviour. Default: `"default"`.
    #[serde(default = "default_zeroclaw_agent_alias")]
    pub agent_alias: String,
    /// Per-request HTTP timeout (ms) for non-streaming calls.
    #[serde(default = "default_gateway_timeout")]
    pub request_timeout_ms: u64,
    /// When true, prefer `/ws/chat` (streamed `done` + parallel `/api/events`
    /// correlation) over `POST /webhook`. Both paths return the same content;
    /// WS is friendlier when ZeroClaw later gains per-token streaming. When
    /// false the plugin uses `/webhook` for every send.
    #[serde(default = "default_true")]
    pub prefer_streaming: bool,
    /// Memory category used by the bidirectional context pusher and any
    /// memory writes from the chat plugin. Show up under `GET /api/memory`
    /// when filtered by this string.
    #[serde(default = "default_zeroclaw_memory_category")]
    pub memory_category: String,
    /// Active ZeroClaw session id (`gw_<uuid>` on disk; `<uuid>` on the wire).
    /// Persisted across runs so reopening the avatar resumes the same
    /// conversation. Empty = mint a fresh uuid on next start and write it
    /// back here via `Settings::save_user`.
    #[serde(default)]
    pub active_session_id: String,
    /// Client-side rolling history window. ZeroClaw chat persists each
    /// session in its sqlite store, so 0 is a sensible default here — the
    /// agent already has the full transcript on the server side. Leave a
    /// small positive value as a belt-and-braces hint in case the persisted
    /// transcript was truncated.
    #[serde(default = "default_zeroclaw_history_window")]
    pub history_window: u32,
    /// Maximum sessions surfaced in the chat sidebar (listed as "threads"
    /// even though ZeroClaw calls them sessions internally).
    #[serde(default = "default_zeroclaw_session_limit")]
    pub session_list_limit: u32,
    /// Enable the bidirectional context pusher (`zeroclaw_context` plugin).
    /// Throttled writes to `POST /api/memory` describing avatar state
    /// (pose, emotion, A2F status, look-at target, recent pose screenshot).
    #[serde(default = "default_true")]
    pub context_push_enabled: bool,
    /// Minimum milliseconds between consecutive memory writes for the same
    /// key, to coalesce noisy state changes.
    #[serde(default = "default_zeroclaw_context_throttle_ms")]
    pub context_throttle_ms: u64,
    /// Embed image URLs in outbound chat text when attachments are present.
    /// ZeroClaw's `/webhook` and `/ws/chat` have no binary payload field; the
    /// `zeroclaw_attachments` plugin hosts a small HTTP server that ZeroClaw
    /// fetches with its built-in HTTP tool.
    #[serde(default = "default_true")]
    pub attachments_enabled: bool,
    /// Address the attachments HTTP server binds to. Must be reachable from
    /// the ZeroClaw host (use `0.0.0.0:6124` over the LAN, or a tunnelled
    /// address when ZeroClaw is remote).
    #[serde(default = "default_zeroclaw_attachments_bind")]
    pub attachments_bind: String,
    /// Public base URL the attachments server is reachable at FROM the
    /// ZeroClaw gateway's perspective. Empty = derive `http://<lan-ip>:<port>`
    /// from [`Self::attachments_bind`].
    #[serde(default)]
    pub attachments_public_url: String,
    /// Maximum number of recently-served attachments kept on disk. Older
    /// files are deleted as new ones land.
    #[serde(default = "default_zeroclaw_attachments_max")]
    pub attachments_max: u32,
}

impl Default for ZeroClawSettings {
    fn default() -> Self {
        Self {
            base_url: default_zeroclaw_base_url(),
            ws_url: String::new(),
            auth_token: String::new(),
            webhook_secret: String::new(),
            client_id: default_zeroclaw_client_id(),
            agent_alias: default_zeroclaw_agent_alias(),
            request_timeout_ms: default_gateway_timeout(),
            prefer_streaming: true,
            active_session_id: String::new(),
            memory_category: default_zeroclaw_memory_category(),
            history_window: default_zeroclaw_history_window(),
            session_list_limit: default_zeroclaw_session_limit(),
            context_push_enabled: true,
            context_throttle_ms: default_zeroclaw_context_throttle_ms(),
            attachments_enabled: true,
            attachments_bind: default_zeroclaw_attachments_bind(),
            attachments_public_url: String::new(),
            attachments_max: default_zeroclaw_attachments_max(),
        }
    }
}

impl ZeroClawSettings {
    /// Trimmed base URL with no trailing slash, suitable for `format!("{base}/path")`.
    pub fn normalized_base_url(&self) -> String {
        self.base_url.trim().trim_end_matches('/').to_string()
    }

    /// Resolve the WebSocket base. Honors [`Self::ws_url`] when set, otherwise
    /// flips the scheme on [`Self::base_url`].
    pub fn resolved_ws_url(&self) -> String {
        let trimmed = self.ws_url.trim();
        if !trimmed.is_empty() {
            return trimmed.trim_end_matches('/').to_string();
        }
        let base = self.normalized_base_url();
        if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            // Bare host:port — assume plain WS.
            format!("ws://{base}")
        }
    }
}

fn default_zeroclaw_base_url() -> String {
    "http://192.168.4.8:42617".to_string()
}

fn default_zeroclaw_client_id() -> String {
    "jarvis-avatar".to_string()
}

fn default_zeroclaw_agent_alias() -> String {
    "default".to_string()
}

fn default_zeroclaw_memory_category() -> String {
    "jarvis-avatar".to_string()
}

fn default_zeroclaw_history_window() -> u32 {
    6
}

fn default_zeroclaw_session_limit() -> u32 {
    50
}

fn default_zeroclaw_context_throttle_ms() -> u64 {
    1_000
}

fn default_zeroclaw_attachments_bind() -> String {
    "0.0.0.0:6124".to_string()
}

fn default_zeroclaw_attachments_max() -> u32 {
    64
}

fn default_gateway_timeout() -> u64 {
    15_000
}
fn default_history_limit() -> u32 {
    50
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TtsSettings {
    pub kokoro_url: String,
    pub voice: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Kokoro `response_format`: `wav`, `pcm`, `mp3`, `opus`, `flac`, … (`pcm` = raw s16le mono for A2F).
    #[serde(default = "default_tts_response_format")]
    pub response_format: String,
    /// Kokoro `stream`. **`false`** avoids chunked WAV/PCM that breaks `hound` / A2F one-shot decode.
    #[serde(default = "default_tts_stream")]
    pub stream: bool,
    /// Sample rate when `response_format` is `pcm` (Kokoro default **24000**).
    #[serde(default = "default_kokoro_pcm_sample_rate")]
    pub pcm_sample_rate: u32,
}

fn default_true() -> bool {
    true
}

fn default_pose_dock_side() -> String {
    "right".to_string()
}

fn default_pose_dock_width() -> f32 {
    520.0
}

fn default_pose_dock_bottom_height() -> f32 {
    280.0
}

fn default_pose_tools_toolbar_pos() -> String {
    "top".to_string()
}

fn default_anim_layers_dock_side() -> String {
    "bottom".to_string()
}

fn default_anim_layers_bottom_height() -> f32 {
    260.0
}

fn default_tts_response_format() -> String {
    "wav".to_string()
}

fn default_tts_stream() -> bool {
    false
}

fn default_kokoro_pcm_sample_rate() -> u32 {
    24_000
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AvatarSettings {
    pub model_path: String,
    /// Default idle loop relative to `assets/` (e.g. `models/idle_loop.vrma`). Empty string
    /// disables autoplay. Arm/elbow twist while a clip runs but not at bind pose is governed by
    /// `bevy_vrm1` VRMA humanoid retarget (see VRMC_vrm_animation pose transform), not by
    /// `lock_root_*` / `lock_vrm_root_y`.
    pub idle_vrma_path: String,
    /// World translation for the VRM root entity (pulls the rig toward the orbit focus).
    pub world_position: [f32; 3],
    /// Uniform scale on the VRM root (1.0 = natural meters). Increase if the rig looks
    /// tiny vs the ground plane; decrease if she is huge.
    #[serde(default = "default_one")]
    pub uniform_scale: f32,
    /// If true, after each VRMA tick snap hips **local** X/Z translation to the bone’s
    /// `RestTransform` bind pose, removing horizontal translation delta while preserving other
    /// axes. Uses rest values, not literal zero — zeroing was incorrect and could make motion
    /// unlike other VRM viewers.
    #[serde(default = "default_true")]
    pub lock_root_xz: bool,
    /// Same as `lock_root_xz` but for Y: snap hips local Y to the bind pose each frame.
    /// Defaults to true — VRMA retarget math in `bevy_vrm1` produces visible vertical drift
    /// for some clips; disable if you explicitly want the hips Y translation from the clip.
    #[serde(default = "default_true")]
    pub lock_root_y: bool,
    /// Hard clamp on the VRM **root entity's** local `Transform.translation.y`, forcing it
    /// back to `world_position.y` after `AnimationSystems`. Catches sliding caused by anything
    /// translating the VRM scene root (as opposed to the hips bone) — independent of the
    /// hips-level `lock_root_xz` / `lock_root_y` knobs.
    #[serde(default = "default_true")]
    pub lock_vrm_root_y: bool,
    pub background_color: [f32; 4],
    pub window_width: u32,
    pub window_height: u32,
    /// If true, when a VRM reaches `Initialized`, load `config/spring_presets/<vrm_key>.toml`
    /// when that file exists (see `plugins/spring_preset.rs` for how `vrm_key` is derived).
    /// Off by default — use Rig editor export/import for explicit workflows.
    #[serde(default)]
    pub auto_load_spring_preset: bool,
    /// Apply `config/ModelOverrides/{stem}/avatar_defaults.json` once the VRM
    /// and bone index are ready (expressions, optional pose, layer set / idle clip).
    #[serde(default = "default_true")]
    pub auto_apply_avatar_defaults: bool,
    /// When true, base idle comes from the animation layer stack (`avatar_defaults.idle_clip`)
    /// instead of spawning the VRMA child from `idle_vrma_path`.
    #[serde(default)]
    pub idle_use_layer_stack: bool,
}

fn default_one() -> f32 {
    1.0
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CameraSettings {
    /// Orbit focus before the VRM is located; also fallback if `focus_follow_vrm` is false.
    pub focus: [f32; 3],
    pub initial_radius: f32,
    pub min_radius: f32,
    pub max_radius: f32,
    pub orbit_sensitivity: f32,
    pub pan_sensitivity: f32,
    pub zoom_sensitivity: f32,
    /// Move orbit focus this far above the VRM root’s world position (typical ~eye/chest).
    pub focus_y_lift: f32,
    /// After load, snap orbit focus to the VRM root so the camera is not stuck on empty space.
    pub focus_follow_vrm: bool,
    /// Frames to wait after `Vrm` exists before reading `GlobalTransform` (scene propagation).
    pub snap_wait_frames: u32,
    /// `0.0` = instant camera response; default plugin uses heavy smoothing.
    pub orbit_smoothness: f32,
    pub zoom_smoothness: f32,
    pub pan_smoothness: f32,
    /// Perspective near-clip distance (meters). Anything closer than this to the
    /// camera gets clipped. Default 0.1 in Bevy is too aggressive for a VRM at
    /// arm's length — drop to ~0.01 to keep her face intact when zoomed in.
    #[serde(default = "default_near_clip")]
    pub near_clip: f32,
    /// Perspective far-clip distance (meters).
    #[serde(default = "default_far_clip")]
    pub far_clip: f32,
    /// Vertical FOV (radians). Default ~π/4 (45°).
    #[serde(default = "default_fov")]
    pub fov_y_radians: f32,
    /// When `true`, any orbit (LMB drag) or zoom (scroll) input re-snaps the
    /// camera focus back to the VRM root. Pan is preserved until the next
    /// orbit/zoom interaction.
    #[serde(default = "default_true")]
    pub recenter_on_orbit_zoom: bool,
    /// When `true`, an LMB press over the model sets the orbit pivot to the
    /// nearest bone joint along the click ray, so the next drag orbits
    /// around that point. Defaults to `false`: PanOrbitCamera always re-aims
    /// the camera at `focus`, so changing the pivot mid-session means the
    /// camera silently re-orients toward the new pivot — and the first drag
    /// frame then sweeps a wide arc around it, which reads as "the camera
    /// shoots over". Until we have a proper trackball-orbit implementation
    /// (rotate around pivot without changing focus), this stays opt-in.
    #[serde(default)]
    pub click_pivot_orbit: bool,
}

fn default_near_clip() -> f32 {
    0.01
}
fn default_far_clip() -> f32 {
    1000.0
}
fn default_fov() -> f32 {
    std::f32::consts::FRAC_PI_4
}

/// Maps `config/user.toml` / UI strings to Bevy [`PresentMode`].
pub fn parse_present_mode(s: &str) -> PresentMode {
    match s.trim().to_ascii_lowercase().as_str() {
        "auto_vsync" | "autovsync" => PresentMode::AutoVsync,
        "auto_no_vsync" | "autonovsync" => PresentMode::AutoNoVsync,
        "fifo" => PresentMode::Fifo,
        "fifo_relaxed" | "fiforelaxed" => PresentMode::FifoRelaxed,
        "immediate" => PresentMode::Immediate,
        "mailbox" => PresentMode::Mailbox,
        _ => PresentMode::Fifo,
    }
}

fn default_present_mode_string() -> String {
    "Fifo".to_string()
}

/// Maps `graphics.msaa_samples` from config/UI to Bevy [`Msaa`].
///
/// * `0` or `1` → off (`Msaa::Off`; Bevy historically used `1` as the “off”
///   sample count in [`Msaa::from_samples`], we treat both as off).
/// * `2` / `4` / `8` → multisampling.
/// * Other values snap to the nearest supported tier (Bevy does not support 3/5/6/7).
pub fn msaa_from_settings(samples: u32) -> Msaa {
    match samples {
        0 | 1 => Msaa::Off,
        2 => Msaa::Sample2,
        4 => Msaa::Sample4,
        8 => Msaa::Sample8,
        3 => Msaa::Sample2,
        5 | 6 | 7 => Msaa::Sample4,
        _ => Msaa::Sample8,
    }
}

/// Bevy’s SSAO pass requires [`Msaa::Off`] on the same camera — keep this false
/// whenever multisampling is active (`msaa_samples` ≥ 2).
#[inline]
pub fn msaa_allows_ssao(samples: u32) -> bool {
    samples <= 1
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GraphicsSettings {
    /// `0` = MSAA off (allows SSAO). `2` / `4` / `8` = multisampling (SSAO auto-disabled).
    pub msaa_samples: u32,
    /// Swapchain present mode (`Fifo` = classic VSync, no tearing on most GPUs).
    /// See [`parse_present_mode`] for accepted spellings. Applies live when changed
    /// from the Graphics window (unlike `msaa_samples`, which needs a restart).
    #[serde(default = "default_present_mode_string")]
    pub present_mode: String,
    pub hdr: bool,
    pub exposure_ev100: f32,
    pub ambient_brightness: f32,
    pub ambient_color: [f32; 4],
    pub directional_illuminance: f32,
    pub directional_shadows: bool,
    pub directional_position: [f32; 3],
    pub directional_look_at: [f32; 3],
    pub show_ground_plane: bool,
    pub ground_size: f32,
    /// Linear RGB base color for the ground plane (very dark recommended).
    pub ground_base_color: [f32; 3],
    /// Tonemapping / bloom / anti-alias knobs — everything behind the "Graphics
    /// Advanced" window. Defaults match Bevy's post-process defaults (TonyMcMapface
    /// + bloom off + SMAA Medium) and can be bumped by `user.toml`.
    #[serde(default)]
    pub advanced: GraphicsAdvancedSettings,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GraphicsAdvancedSettings {
    #[serde(default = "default_tonemap")]
    pub tonemapping: String,
    #[serde(default)]
    pub bloom: BloomSettings,
    #[serde(default = "default_smaa_preset")]
    pub smaa_preset: String,
    #[serde(default)]
    pub fxaa_enabled: bool,
    /// If true, attach `AutoExposure` to the camera (requires HDR + compute shaders).
    #[serde(default)]
    pub auto_exposure: bool,
    /// Screen-space ambient occlusion on the main camera (crevice read). Not
    /// supported on WebGL2 / WebGPU; pairs best with HDR. **Incompatible with
    /// MSAA** (Bevy requires `Msaa::Off` on the camera) — use `msaa_samples = 0`
    /// when SSAO is enabled.
    #[serde(default)]
    pub ssao_enabled: bool,
    /// One of: Low, Medium, High, Ultra (see Bevy `ScreenSpaceAmbientOcclusionQualityLevel`).
    #[serde(default = "default_ssao_quality")]
    pub ssao_quality: String,
    /// Bevy `ScreenSpaceAmbientOcclusion::constant_object_thickness` — lower reads
    /// tighter crevice contact; higher avoids self-occlusion on curved surfaces.
    #[serde(default = "default_ssao_constant_object_thickness")]
    pub ssao_constant_object_thickness: f32,
    /// Optional environment-map cube stem relative to `assets/` (e.g. `envmaps/studio`).
    /// Looks for `<stem>_diffuse.ktx2` + `<stem>_specular.ktx2`; ignored when empty.
    #[serde(default)]
    pub environment_map: String,
    /// Diffuse/specular IBL strength in approximate **nits** (cd/m²). Filament-style
    /// environment maps: typical indoor scenes ~5–20, bright studio ~20–50.
    /// Legacy configs used values in the hundreds/thousands; see `sync_environment_map`.
    #[serde(default = "default_env_intensity")]
    pub environment_intensity: f32,
    /// Extra multiplier on camera IBL for MToon (toon shading hides indirect light).
    #[serde(default = "default_env_mtoon_boost")]
    pub environment_map_mtoon_boost: f32,
    /// Multiplier on [`GraphicsSettings::ambient_brightness`] while the view
    /// environment map is attached (flat ambient otherwise hides cubemap IBL).
    #[serde(default = "default_env_ambient_scale_when_active")]
    pub environment_ambient_scale_when_active: f32,
    /// Yaw rotation (degrees) applied to the view environment cubemap.
    #[serde(default)]
    pub environment_map_rotation_yaw_deg: f32,
    /// When false, cubemaps are not attached to the camera (sliders still edit settings).
    #[serde(default = "default_true")]
    pub environment_map_enabled: bool,
    /// Spawn a white PBR sphere beside the avatar to verify IBL on standard materials.
    #[serde(default)]
    pub environment_map_debug_sphere: bool,
    /// Replace MToon indirect with raw cubemap tint (confirms shader sampling).
    #[serde(default)]
    pub environment_map_debug_visualize: bool,
    /// Rotate the view IBL cubemap with the camera (skybox-style) plus `rotation_yaw_deg`.
    #[serde(default = "default_true")]
    pub environment_map_follow_camera: bool,
    /// Extra multiplier on MToon-only cubemap samples in `mtoon_fragment.wgsl` (PBR sphere ignores this).
    #[serde(default = "default_env_mtoon_body_gain")]
    pub environment_map_mtoon_body_gain: f32,
}

impl GraphicsAdvancedSettings {
    /// Diffuse/specular IBL strength sent to [`bevy::light::EnvironmentMapLight`].
    pub fn environment_nits(&self) -> f32 {
        environment_intensity_nits(self.environment_intensity)
            * self.environment_map_mtoon_boost.max(0.1)
    }

    pub fn environment_rotation(&self, camera_rotation: Quat) -> Quat {
        let yaw = Quat::from_rotation_y(self.environment_map_rotation_yaw_deg.to_radians());
        if self.environment_map_follow_camera {
            camera_rotation * yaw
        } else {
            yaw
        }
    }
}

impl GraphicsSettings {
    pub fn effective_ambient_brightness(&self, ibl_active: bool) -> f32 {
        let scale = if ibl_active {
            self.advanced
                .environment_ambient_scale_when_active
                .clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.ambient_brightness * scale
    }
}

/// Converts UI `environment_intensity` to approximate nits (cd/m²). Slider is linear nits.
pub fn environment_intensity_nits(raw: f32) -> f32 {
    raw.max(0.0)
}

impl Default for GraphicsAdvancedSettings {
    fn default() -> Self {
        Self {
            tonemapping: default_tonemap(),
            bloom: BloomSettings::default(),
            smaa_preset: default_smaa_preset(),
            fxaa_enabled: false,
            auto_exposure: false,
            ssao_enabled: false,
            ssao_quality: default_ssao_quality(),
            ssao_constant_object_thickness: default_ssao_constant_object_thickness(),
            environment_map: String::new(),
            environment_intensity: default_env_intensity(),
            environment_map_mtoon_boost: default_env_mtoon_boost(),
            environment_ambient_scale_when_active: default_env_ambient_scale_when_active(),
            environment_map_rotation_yaw_deg: 0.0,
            environment_map_enabled: true,
            environment_map_debug_sphere: false,
            environment_map_debug_visualize: false,
            environment_map_follow_camera: true,
            environment_map_mtoon_body_gain: default_env_mtoon_body_gain(),
        }
    }
}

fn default_env_mtoon_boost() -> f32 {
    2.5
}

fn default_env_mtoon_body_gain() -> f32 {
    4.0
}

fn default_env_ambient_scale_when_active() -> f32 {
    0.3
}

fn default_tonemap() -> String {
    "TonyMcMapface".to_string()
}
fn default_smaa_preset() -> String {
    "Medium".to_string()
}
fn default_ssao_quality() -> String {
    "High".to_string()
}

fn default_ssao_constant_object_thickness() -> f32 {
    0.25
}
fn default_env_intensity() -> f32 {
    12.0
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BloomSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_bloom_intensity")]
    pub intensity: f32,
    #[serde(default = "default_bloom_lfb")]
    pub low_frequency_boost: f32,
    #[serde(default = "default_bloom_hpf")]
    pub high_pass_frequency: f32,
    #[serde(default = "default_bloom_threshold")]
    pub threshold: f32,
    #[serde(default = "default_bloom_softness")]
    pub threshold_softness: f32,
    /// Either `"energy_conserving"` (default) or `"additive"`.
    #[serde(default = "default_bloom_mode")]
    pub composite_mode: String,
}

impl Default for BloomSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: default_bloom_intensity(),
            low_frequency_boost: default_bloom_lfb(),
            high_pass_frequency: default_bloom_hpf(),
            threshold: default_bloom_threshold(),
            threshold_softness: default_bloom_softness(),
            composite_mode: default_bloom_mode(),
        }
    }
}

fn default_bloom_intensity() -> f32 {
    0.15
}
fn default_bloom_lfb() -> f32 {
    0.7
}
fn default_bloom_hpf() -> f32 {
    1.0
}
fn default_bloom_threshold() -> f32 {
    0.0
}
fn default_bloom_softness() -> f32 {
    0.0
}
fn default_bloom_mode() -> String {
    "energy_conserving".to_string()
}

/// Pose Controller defaults (idle + transition knobs).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoseControllerSettings {
    #[serde(default)]
    pub idle_enabled: bool,
    #[serde(default = "default_idle_interval_min")]
    pub idle_interval_min_sec: f32,
    #[serde(default = "default_idle_interval_max")]
    pub idle_interval_max_sec: f32,
    /// Category filter applied to idle picks. Empty string = no filter.
    #[serde(default)]
    pub idle_category: String,
    #[serde(default = "default_transition_seconds")]
    pub default_transition_seconds: f32,
    #[serde(default = "default_blend_weight")]
    pub default_blend_weight: f32,
    /// Honour per-command `blend_weight` / `transition_seconds`. When false,
    /// `apply_pose_commands` stays on its historical "instant set" path.
    #[serde(default)]
    pub blend_transitions_enabled: bool,
    /// Automatically stop every `Vrma` animation player whenever a manual
    /// pose / expression command lands. Without this the idle VRMA keeps
    /// sampling bone transforms every frame and overwrites our writes.
    #[serde(default = "default_auto_stop_idle_vrma")]
    pub auto_stop_idle_vrma: bool,
}

impl Default for PoseControllerSettings {
    fn default() -> Self {
        Self {
            idle_enabled: false,
            idle_interval_min_sec: default_idle_interval_min(),
            idle_interval_max_sec: default_idle_interval_max(),
            idle_category: String::new(),
            default_transition_seconds: default_transition_seconds(),
            default_blend_weight: default_blend_weight(),
            blend_transitions_enabled: false,
            auto_stop_idle_vrma: default_auto_stop_idle_vrma(),
        }
    }
}

fn default_idle_interval_min() -> f32 {
    8.0
}
fn default_idle_interval_max() -> f32 {
    18.0
}
fn default_transition_seconds() -> f32 {
    0.35
}
fn default_blend_weight() -> f32 {
    1.0
}
fn default_auto_stop_idle_vrma() -> bool {
    true
}

/// Four-light anime rig (key / fill / rim / back) spawned at startup. Each
/// sub-struct maps to a `DirectionalLight` entity.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LightRigSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Draw Bevy directional-light arrows in the viewport (Blender-style).
    #[serde(default = "default_true")]
    pub show_light_gizmos: bool,
    /// How far from the rig focus point each gizmo anchor sits along the light
    /// direction (meters). Does not affect lighting — only gizmo placement.
    #[serde(default = "default_light_gizmo_distance")]
    pub gizmo_distance: f32,
    /// Anchor gizmos on the loaded VRM root + `camera.focus_y_lift` instead of
    /// the static `[camera].focus` point.
    #[serde(default = "default_true")]
    pub use_avatar_focus_for_gizmos: bool,
    #[serde(default)]
    pub key: LightSpec,
    #[serde(default = "default_fill_light")]
    pub fill: LightSpec,
    #[serde(default = "default_rim_light")]
    pub rim: LightSpec,
    /// Dedicated backlight behind the character (hair / cape / silhouette).
    #[serde(default = "default_back_light")]
    pub back: LightSpec,
}

impl Default for LightRigSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            show_light_gizmos: true,
            gizmo_distance: default_light_gizmo_distance(),
            use_avatar_focus_for_gizmos: true,
            key: LightSpec::default(),
            fill: default_fill_light(),
            rim: default_rim_light(),
            back: default_back_light(),
        }
    }
}

fn default_light_gizmo_distance() -> f32 {
    2.5
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LightSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Direction the light points AT from its notional position (world space).
    pub direction: [f32; 3],
    /// Linear RGB color.
    pub color: [f32; 3],
    pub illuminance: f32,
    /// MToon shading only reacts to directional lights with shadows enabled —
    /// leave the key light at `true` unless you know what you're doing.
    #[serde(default)]
    pub shadows: bool,
}

impl Default for LightSpec {
    fn default() -> Self {
        // Warm, shadow-casting key light in front-right.
        Self {
            enabled: true,
            direction: [-0.6, -1.0, -0.8],
            color: [1.0, 0.96, 0.90],
            illuminance: 9000.0,
            shadows: true,
        }
    }
}

fn default_fill_light() -> LightSpec {
    LightSpec {
        enabled: true,
        direction: [0.8, -0.4, -0.6],
        color: [0.75, 0.85, 1.0],
        illuminance: 3500.0,
        shadows: false,
    }
}

fn default_rim_light() -> LightSpec {
    LightSpec {
        enabled: true,
        // Above-behind the avatar (negative Z = behind in our VRM facing).
        direction: [-0.25, -0.55, -1.0],
        color: [1.0, 0.88, 0.78],
        illuminance: 7500.0,
        shadows: false,
    }
}

fn default_back_light() -> LightSpec {
    LightSpec {
        enabled: true,
        direction: [0.0, -0.12, -1.0],
        color: [0.92, 0.94, 1.0],
        illuminance: 6500.0,
        shadows: false,
    }
}

/// One-click lighting + post preset aimed at high-contrast character showcase
/// (Girls' Frontline Exilium 2–style rim/back separation).
pub fn apply_character_showcase_lighting_preset(settings: &mut Settings) {
    let g = &mut settings.graphics;
    g.ambient_brightness = 0.12;
    g.ambient_color = [0.62, 0.66, 0.82, 1.0];
    g.exposure_ev100 = 10.2;

    let adv = &mut g.advanced;
    adv.tonemapping = "AgX".to_string();
    adv.bloom.enabled = true;
    adv.bloom.intensity = 0.22;
    adv.bloom.threshold = 0.85;
    adv.bloom.threshold_softness = 0.35;
    adv.bloom.low_frequency_boost = 0.85;
    adv.environment_intensity = 18.0;
    adv.environment_map_mtoon_boost = 2.5;

    let rig = &mut settings.light_rig;
    rig.enabled = true;
    rig.show_light_gizmos = true;
    rig.gizmo_distance = 2.8;
    rig.key.illuminance = 8500.0;
    rig.fill.illuminance = 2800.0;
    rig.rim = default_rim_light();
    rig.rim.enabled = true;
    rig.rim.illuminance = 9000.0;
    rig.back = default_back_light();
    rig.back.enabled = true;
    rig.back.illuminance = 8000.0;
}

/// Per-material MToon overrides (written to disk as a JSON sidecar). The
/// `MToonOverridesPlugin` loads this file on boot and applies it to any
/// material whose `Name` matches.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MToonOverridesSettings {
    #[serde(default = "default_mtoon_override_path")]
    pub path: String,
}

impl Default for MToonOverridesSettings {
    fn default() -> Self {
        Self {
            path: default_mtoon_override_path(),
        }
    }
}

fn default_mtoon_override_path() -> String {
    "config/mtoon_overrides.json".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LookAtSettings {
    pub idle_return_speed: f32,
}

impl Settings {
    /// Load `config/default.toml`, overlay `config/user.toml` if present, then apply
    /// `JARVIS__*` env vars on top.
    pub fn load() -> Result<Self, config::ConfigError> {
        Config::builder()
            .add_source(File::with_name(DEFAULT_CONFIG_STEM))
            .add_source(File::with_name(USER_CONFIG_STEM).required(false))
            .add_source(
                Environment::with_prefix("JARVIS")
                    .try_parsing(true)
                    .separator("__"),
            )
            .build()?
            .try_deserialize()
    }

    /// Write the full current [`Settings`] snapshot to `config/user.toml`. This is what the
    /// debug UI's "Save settings" button calls — it preserves the factory `default.toml` as a
    /// baseline and only overlays this user snapshot on top.
    pub fn save_user(&self) -> Result<(), String> {
        let body = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(parent) = std::path::Path::new(USER_CONFIG_PATH).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        std::fs::write(USER_CONFIG_PATH, body).map_err(|e| e.to_string())
    }

    /// Delete `config/user.toml` (if it exists) and reload a fresh [`Settings`] from the
    /// remaining sources. "Not found" is treated as success so the caller always gets a
    /// clean factory snapshot back.
    pub fn restore_defaults() -> Result<Self, String> {
        match std::fs::remove_file(USER_CONFIG_PATH) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        Self::load().map_err(|e| e.to_string())
    }

    /// One-shot migration: open consolidated workspaces when related standalone
    /// panels were already enabled in saved settings.
    pub fn migrate_workspace_visibility(&mut self) {
        let u = &mut self.ui;

        if !u.show_graphics_workspace && u.show_graphics_advanced {
            u.show_graphics_workspace = true;
        }

        if !u.show_diagnostics_workspace && u.show_network_trace {
            u.show_diagnostics_workspace = true;
        }
    }
}
