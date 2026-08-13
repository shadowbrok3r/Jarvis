//! Android app: Bevy owns the NativeActivity event loop and drives the same
//! plugin stack as the desktop binary, minus the modules gated out in
//! `jarvis_avatar::plugins`.

mod android_boot;
mod asset_bootstrap;
mod hub_client;
#[cfg(feature = "ime-bridge")]
mod ime;
mod render_probe;
mod ui;

use bevy::asset::io::AssetSourceBuilder;
use bevy::asset::io::file::FileAssetReader;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{Backends, RenderCreation, WgpuSettings};
use bevy_egui::EguiPlugin;
use bevy_vrm1::prelude::*;
use jarvis_avatar::config::Settings;
use jarvis_avatar::plugins;

/// Compiled-in factory config; the device overlay is `<internal>/user.toml`.
const DEFAULT_CONFIG: &str = include_str!("../../../config/default.toml");

#[bevy_main]
fn main() {
    let ctx = android_boot::init();
    let staged = asset_bootstrap::extract_if_needed(&ctx);

    let overlays = android_boot::user_overlays(&ctx);
    let refs: Vec<&str> = overlays.iter().map(String::as_str).collect();
    let mut settings = Settings::load_layered(DEFAULT_CONFIG, &refs).unwrap_or_else(|e| {
        log::error!("settings load failed, using compiled defaults: {e}");
        Settings::load_layered(DEFAULT_CONFIG, &[]).expect("compiled-in default.toml parses")
    });
    settings.avatar.model_path =
        android_boot::resolve_model_path(&ctx, &settings.avatar.model_path, &staged);
    android_boot::redirect_pose_library(&ctx, &mut settings);
    android_boot::clamp_graphics(&mut settings);
    log::info!("model={} idle={}", settings.avatar.model_path, settings.avatar.idle_vrma_path);

    let assets_root = ctx.assets_dir.clone();
    let mut app = App::new();

    // Must precede `DefaultPlugins`: Bevy's Android reader ignores
    // `AssetPlugin.file_path` and serves only from inside the APK.
    app.register_asset_source(
        bevy::asset::io::AssetSourceId::Default,
        AssetSourceBuilder::new(move || Box::new(FileAssetReader::new(assets_root.clone()))),
    );

    // Our EditText bridge owns the soft keyboard when enabled; bevy_egui's
    // `process_ime_system` would call winit `set_ime_allowed`, whose Android
    // mapping is an *implicit* DecorView show/hide — the hide kills a keyboard
    // served by the EditText, and the following show is then ignored.
    #[cfg(feature = "ime-bridge")]
    app.insert_resource(bevy_egui::EguiGlobalSettings {
        enable_ime: false,
        ..default()
    });

    app.insert_resource(settings)
        .add_plugins(DefaultPlugins.set(RenderPlugin {
            // Bevy's facade does not re-export bevy_render's `gles` feature, so
            // Vulkan is the only backend available here anyway; pinning it keeps
            // adapter selection off the GLES fallback path.
            render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                backends: Some(Backends::VULKAN),
                ..default()
            })),
            ..default()
        }))
        .add_plugins((VrmPlugin, VrmaPlugin))
        .add_plugins((
            plugins::traffic_log::TrafficLogPlugin,
            plugins::shared_runtime::SharedRuntimePlugin,
            // Hub message types without the axum listener; expressions /
            // look_at / alive_director read and write them.
            plugins::channel_server::HubMessagesPlugin,
            plugins::environment::EnvironmentPlugin,
            // `EguiPlugin` must precede `PanOrbitCameraPlugin` (added by
            // `OrbitCameraPlugin`) so `check_egui_wants_focus` orders after
            // egui's pre-update set.
            EguiPlugin::default(),
            // Owns `RigEditorState`, which `orbit_camera` reads. Its gizmo
            // system is Android-gated; the resource is what matters here.
            plugins::rig_editor::RigEditorPlugin,
            plugins::orbit_camera::OrbitCameraPlugin,
            plugins::avatar::AvatarPlugin,
            plugins::avatar_defaults::AvatarDefaultsPlugin,
        ))
        .add_plugins((
            plugins::expressions::ExpressionsPlugin,
            plugins::look_at::LookAtPlugin,
            plugins::spring_bone::SpringBonePlugin,
            plugins::pose_driver::PoseDriverPlugin,
            plugins::pose_library_assets::PoseLibraryAssetsPlugin,
            plugins::native_anim_player::NativeAnimPlayerPlugin,
            plugins::anim_layers::AnimLayersPlugin,
            plugins::anim_layer_sets::AnimLayerSetsPlugin,
        ))
        .add_plugins((
            plugins::light_rig::LightRigPlugin,
            plugins::graphics_advanced::GraphicsAdvancedPlugin,
            plugins::mtoon_overrides::MToonOverridesPlugin,
            plugins::material_visibility::MaterialVisibilityPlugin,
            plugins::bone_influence::BoneInfluencePlugin,
            plugins::idle_tick::IdleTickPlugin,
            plugins::alive_director::AliveDirectorPlugin,
            plugins::emotion_map::EmotionMapPlugin,
            // Kokoro HTTP -> bevy_audio, plus A2F visemes. Reads
            // `settings.tts.kokoro_url`, so the phone must reach that LAN host.
            plugins::tts::TtsPlugin,
        ))
        .add_plugins((
            // Turns hub `WsIncomingMessage` envelopes into pose commands. Inert
            // until a hub client feeds that message; the desktop drives it.
            plugins::hub_pose_apply::HubPoseApplyPlugin,
            plugins::vrma_clip_import::VrmaClipImportPlugin,
            // HTTP probes of the hub / gateway / Kokoro for a status readout.
            plugins::service_status::ServiceStatusPlugin,
        ))
        .add_plugins((
            render_probe::RenderProbePlugin,
            ui::AndroidUiPlugin,
            hub_client::HubClientPlugin,
        ));

    // Credentialed services are registered only when their credential is present,
    // so a phone without the pushed external `user.toml` never spawns their
    // threads or sockets at all. See `android_boot::user_overlays`.
    let cfg = app.world().resource::<Settings>().clone();
    if !cfg.gateway.auth_token.is_empty() {
        info!("gateway token present: enabling IronClaw chat");
        app.add_plugins(plugins::ironclaw_chat::IronclawChatPlugin);
    }
    if !cfg.zeroclaw.auth_token.is_empty() {
        info!("zeroclaw token present: enabling ZeroClaw chat");
        app.add_plugins((
            plugins::zeroclaw_chat::ZeroClawChatPlugin,
            plugins::zeroclaw_context::ZeroClawContextPlugin,
            plugins::zeroclaw_attachments::ZeroClawAttachmentsPlugin,
        ));
    }
    if !cfg.home_assistant.ha_token.is_empty() {
        info!("ha token present: enabling Home Assistant");
        app.add_plugins((
            plugins::home_assistant::HomeAssistantPlugin,
            plugins::ha_vision_gaze::HaVisionGazePlugin,
        ));
    }

    #[cfg(feature = "ime-bridge")]
    app.add_plugins(ime::AndroidImePlugin);

    app
        .run();
}
