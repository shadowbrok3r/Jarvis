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
    pub show_graphics: bool,
    #[serde(default)]
    pub show_live_test: bool,
    #[serde(default)]
    pub show_channel_hub: bool,
    #[serde(default)]
    pub show_gateway: bool,
    #[serde(default)]
    pub show_tts: bool,
    #[serde(default)]
    pub show_look_at: bool,
    #[serde(default)]
    pub show_mcp: bool,
    #[serde(default)]
    pub show_pose_controller: bool,
    /// Viewport bone pick, euler gizmo helpers, and VRMC spring joint tuning.
    #[serde(default)]
    pub show_rig_editor: bool,
    #[serde(default)]
    pub show_graphics_advanced: bool,
    #[serde(default = "default_true")]
    pub show_services: bool,
    /// Dedicated "Animation Layers" window — timeline view of every active
    /// layer with per-layer enable / weight / play controls.
    #[serde(default)]
    pub show_anim_layers: bool,
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
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            show_chat: true,
            show_avatar: false,
            show_camera: false,
            show_graphics: false,
            show_live_test: false,
            show_channel_hub: false,
            show_gateway: false,
            show_tts: false,
            show_look_at: false,
            show_mcp: false,
            show_pose_controller: false,
            show_rig_editor: false,
            show_graphics_advanced: false,
            show_services: true,
            show_anim_layers: false,
            show_emotion_mappings: false,
            show_home_assistant: false,
            show_network_trace: false,
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
}

fn default_mcp_path() -> String {
    "/mcp".to_string()
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
}

fn default_true() -> bool {
    true
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
        }
    }
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

/// Three-light "anime" rig spawned at startup. Each sub-struct maps to a
/// `DirectionalLight` entity; disable individually if you want to bring
/// your own lighting.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LightRigSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub key: LightSpec,
    #[serde(default = "default_fill_light")]
    pub fill: LightSpec,
    #[serde(default = "default_rim_light")]
    pub rim: LightSpec,
}

impl Default for LightRigSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            key: LightSpec::default(),
            fill: default_fill_light(),
            rim: default_rim_light(),
        }
    }
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
        direction: [0.2, -0.2, 1.0],
        color: [1.0, 0.9, 0.8],
        illuminance: 5000.0,
        shadows: false,
    }
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
}
