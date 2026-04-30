pub use jarvis_avatar::egui_theme::STYLE;

mod kimodo;
mod mcp;
mod plugins;

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

    App::new()
        .insert_resource(settings)
        .add_plugins(DefaultPlugins)
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
