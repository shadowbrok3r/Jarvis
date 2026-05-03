pub use jarvis_avatar::egui_theme::STYLE;

mod kimodo;
mod mcp;
mod plugins;

use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowResolution};
use bevy_vrm1::prelude::*;
use jarvis_avatar::config::Settings;
use jarvis_avatar::config::parse_present_mode;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    dotenvy::dotenv().ok();

    let mut settings = Settings::load().expect("load config/default.toml (cwd = crate root)");
    if let Ok(t) = std::env::var("IRONCLAW_TOKEN") {
        if !t.is_empty() {
            settings.ironclaw.auth_token = t;
        }
    }
    if let Ok(t) = std::env::var("IRONCLAW_GATEWAY_TOKEN") {
        if !t.is_empty() {
            settings.gateway.auth_token = t;
        }
    }

    let asset_path = resolve_asset_path();
    info!("assets resolved to: {asset_path}");
    if let Some(parent) = std::path::Path::new(&asset_path).parent() {
        // Bevy's file `AssetReader` joins `AssetPlugin.file_path` ("assets" by
        // default) onto `get_base_path()`, which prefers `BEVY_ASSET_ROOT`,
        // then `CARGO_MANIFEST_DIR`, then the executable's parent. Setting
        // `BEVY_ASSET_ROOT` lets the binary find `assets/` regardless of cwd
        // or where the binary was launched from.
        // SAFETY: single-threaded process startup, before `App::new`.
        unsafe {
            std::env::set_var("BEVY_ASSET_ROOT", parent);
        }
    }

    App::new()
        .insert_resource(settings)
        .add_plugins(DefaultPlugins.set(AssetPlugin {
            file_path: asset_path,
            ..default()
        }))
        .add_plugins((
            plugins::traffic_log::TrafficLogPlugin,
            plugins::home_assistant::HomeAssistantPlugin,
        ))
        .add_plugins((
            VrmPlugin,
            VrmaPlugin,
            plugins::shared_runtime::SharedRuntimePlugin,
            plugins::environment::EnvironmentPlugin,
            // `EguiPlugin` must register before `PanOrbitCameraPlugin` when using the
            // `bevy_egui` feature so `check_egui_wants_focus` orders after egui's pre-update set.
            plugins::rig_editor::RigEditorPlugin,
            plugins::debug_ui::DebugUiPlugin,
            plugins::orbit_camera::OrbitCameraPlugin,
            plugins::avatar::AvatarPlugin,
        ))
        .add_plugins((
            plugins::channel_server::ChannelHubPlugin,
            plugins::ha_vision_gaze::HaVisionGazePlugin,
            plugins::ironclaw_chat::IronclawChatPlugin,
            plugins::expressions::ExpressionsPlugin,
            plugins::look_at::LookAtPlugin,
            plugins::spring_bone::SpringBonePlugin,
            plugins::tts::TtsPlugin,
            plugins::pose_driver::PoseDriverPlugin,
            plugins::pose_capture::PoseCapturePlugin,
            mcp::plugin::McpPlugin,
        ))
        .add_plugins((
            plugins::pose_library_assets::PoseLibraryAssetsPlugin,
            plugins::hub_pose_apply::HubPoseApplyPlugin,
            plugins::native_anim_player::NativeAnimPlayerPlugin,
            plugins::light_rig::LightRigPlugin,
            plugins::graphics_advanced::GraphicsAdvancedPlugin,
            plugins::mtoon_overrides::MToonOverridesPlugin,
            plugins::idle_tick::IdleTickPlugin,
            plugins::anim_layers::AnimLayersPlugin,
            plugins::anim_layer_sets::AnimLayerSetsPlugin,
            plugins::emotion_map::EmotionMapPlugin,
            plugins::service_status::ServiceStatusPlugin,
        ))
        .add_plugins(InjectKimodoClientPlugin)
        .add_systems(Startup, configure_primary_window)
        .run();
}

/// Wires the UI-visible [`KimodoClient`] resource from the live
/// [`HubBroadcast`] and [`StreamingAnimation`] after they exist. We do this as
/// a small plugin so `main` stays declarative.
struct InjectKimodoClientPlugin;
impl bevy::app::Plugin for InjectKimodoClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostStartup, inject_kimodo_client);
    }
}

fn inject_kimodo_client(
    mut commands: Commands,
    hub: Option<Res<plugins::channel_server::HubBroadcast>>,
    streaming: Option<Res<plugins::native_anim_player::StreamingAnimation>>,
) {
    if let (Some(hub), Some(streaming)) = (hub, streaming) {
        let client = kimodo::KimodoClient::new(hub.clone()).with_streaming(streaming.clone());
        commands.insert_resource(plugins::debug_ui::KimodoClientRes(client));
    }
}

/// Find the `assets/` directory at runtime.
///
/// Bevy's default `AssetPlugin` resolves assets relative to the binary's
/// parent directory, which works for `cargo run` (binary lives next to the
/// project root) but breaks when launching `target/debug/jarvis-avatar`
/// directly. We instead probe a small set of well-known candidates so the
/// dev binary, an installed binary, and a `cargo run` invocation all behave
/// the same.
///
/// Search order:
///   1. `JARVIS_ASSETS_DIR` env var (explicit override)
///   2. `<cwd>/assets` (typical for `cargo run` from crate root)
///   3. `<exe_dir>/assets` (installed/portable layout)
///   4. `<exe_dir>/../assets`, `../../assets`, `../../../assets`
///      (covers `target/{debug,release}/jarvis-avatar` and similar)
///   5. Compile-time `CARGO_MANIFEST_DIR/assets` (last-resort dev fallback)
fn resolve_asset_path() -> String {
    use std::path::PathBuf;

    if let Ok(p) = std::env::var("JARVIS_ASSETS_DIR") {
        if !p.is_empty() && PathBuf::from(&p).is_dir() {
            return p;
        }
    }

    let cwd_assets = PathBuf::from("assets");
    if cwd_assets.is_dir() {
        return "assets".to_string();
    }

    if let Ok(exe) = std::env::current_exe() {
        let exe_dir = exe.parent().map(PathBuf::from);
        for candidate in [
            exe_dir.clone(),
            exe_dir.as_ref().and_then(|d| d.parent().map(PathBuf::from)),
            exe_dir
                .as_ref()
                .and_then(|d| d.parent().and_then(|p| p.parent()).map(PathBuf::from)),
            exe_dir.as_ref().and_then(|d| {
                d.parent()
                    .and_then(|p| p.parent().and_then(|q| q.parent()))
                    .map(PathBuf::from)
            }),
        ]
        .into_iter()
        .flatten()
        {
            let assets = candidate.join("assets");
            if assets.is_dir() {
                return assets.to_string_lossy().into_owned();
            }
        }
    }

    let manifest_assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    if manifest_assets.is_dir() {
        return manifest_assets.to_string_lossy().into_owned();
    }

    "assets".to_string()
}

fn configure_primary_window(
    mut q: Query<&mut Window, With<PrimaryWindow>>,
    settings: Res<Settings>,
) {
    let Ok(mut window) = q.single_mut() else {
        return;
    };
    window.title = "jarvis-avatar".into();
    window.resolution =
        WindowResolution::new(settings.avatar.window_width, settings.avatar.window_height);
    window.present_mode = parse_present_mode(&settings.graphics.present_mode);
}
