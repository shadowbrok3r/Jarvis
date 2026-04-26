pub const STYLE: &str = r#"{"warn_if_rect_changes_id": false, "override_text_style":null,"override_font_id":null,"override_text_valign":"Center","text_styles":{"Small":{"size":10.0,"family":"Monospace"},"Body":{"size":14.0,"family":"Monospace"},"Monospace":{"size":12.0,"family":"Monospace"},"Button":{"size":14.0,"family":"Monospace"},"Heading":{"size":18.0,"family":"Monospace"}},"drag_value_text_style":"Button","wrap":null,"wrap_mode":null,"spacing":{"item_spacing":{"x":3.0,"y":3.0},"window_margin":{"left":12,"right":12,"top":12,"bottom":12},"button_padding":{"x":5.0,"y":3.0},"menu_margin":{"left":12,"right":12,"top":12,"bottom":12},"indent":18.0,"interact_size":{"x":40.0,"y":20.0},"slider_width":100.0,"slider_rail_height":8.0,"combo_width":100.0,"text_edit_width":280.0,"icon_width":14.0,"icon_width_inner":8.0,"icon_spacing":6.0,"default_area_size":{"x":600.0,"y":400.0},"tooltip_width":600.0,"menu_width":400.0,"menu_spacing":2.0,"indent_ends_with_horizontal_line":false,"combo_height":200.0,"scroll":{"floating":true,"bar_width":6.0,"handle_min_length":12.0,"bar_inner_margin":4.0,"bar_outer_margin":0.0,"floating_width":2.0,"floating_allocated_width":0.0,"foreground_color":true,"dormant_background_opacity":0.0,"active_background_opacity":0.4,"interact_background_opacity":0.7,"dormant_handle_opacity":0.0,"active_handle_opacity":0.6,"interact_handle_opacity":1.0}},"interaction":{"interact_radius":5.0,"resize_grab_radius_side":5.0,"resize_grab_radius_corner":10.0,"show_tooltips_only_when_still":true,"tooltip_delay":0.5,"tooltip_grace_time":0.2,"selectable_labels":true,"multi_widget_text_select":true},"visuals":{"dark_mode":true,"text_alpha_from_coverage":"TwoCoverageMinusCoverageSq","override_text_color":[207,216,220,255],"weak_text_alpha":0.6,"weak_text_color":null,"widgets":{"noninteractive":{"bg_fill":[0,0,0,0],"weak_bg_fill":[61,61,61,232],"bg_stroke":{"width":1.0,"color":[71,71,71,247]},"corner_radius":{"nw":6,"ne":6,"sw":6,"se":6},"fg_stroke":{"width":1.0,"color":[207,216,220,255]},"expansion":0.0},"inactive":{"bg_fill":[58,51,106,0],"weak_bg_fill":[8,8,8,231],"bg_stroke":{"width":1.5,"color":[48,51,73,255]},"corner_radius":{"nw":6,"ne":6,"sw":6,"se":6},"fg_stroke":{"width":1.0,"color":[207,216,220,255]},"expansion":0.0},"hovered":{"bg_fill":[37,29,61,97],"weak_bg_fill":[95,62,97,69],"bg_stroke":{"width":1.7,"color":[106,101,155,255]},"corner_radius":{"nw":6,"ne":6,"sw":6,"se":6},"fg_stroke":{"width":1.5,"color":[83,87,88,35]},"expansion":2.0},"active":{"bg_fill":[12,12,15,255],"weak_bg_fill":[39,37,54,214],"bg_stroke":{"width":1.0,"color":[12,12,16,255]},"corner_radius":{"nw":6,"ne":6,"sw":6,"se":6},"fg_stroke":{"width":2.0,"color":[207,216,220,255]},"expansion":1.0},"open":{"bg_fill":[20,22,28,255],"weak_bg_fill":[17,18,22,255],"bg_stroke":{"width":1.8,"color":[42,44,93,165]},"corner_radius":{"nw":6,"ne":6,"sw":6,"se":6},"fg_stroke":{"width":1.0,"color":[109,109,109,255]},"expansion":0.0}},"selection":{"bg_fill":[23,64,53,27],"stroke":{"width":1.0,"color":[12,12,15,255]}},"hyperlink_color":[135,85,129,255],"faint_bg_color":[17,18,22,255],"extreme_bg_color":[9,12,15,83],"text_edit_bg_color":null,"code_bg_color":[30,31,35,255],"warn_fg_color":[61,185,157,255],"error_fg_color":[255,55,102,255],"window_corner_radius":{"nw":6,"ne":6,"sw":6,"se":6},"window_shadow":{"offset":[0,0],"blur":7,"spread":5,"color":[17,17,41,118]},"window_fill":[11,11,15,255],"window_stroke":{"width":1.0,"color":[77,94,120,138]},"window_highlight_topmost":true,"menu_corner_radius":{"nw":6,"ne":6,"sw":6,"se":6},"panel_fill":[12,12,15,255],"popup_shadow":{"offset":[0,0],"blur":8,"spread":3,"color":[19,18,18,96]},"resize_corner_size":18.0,"text_cursor":{"stroke":{"width":2.0,"color":[197,192,255,255]},"preview":true,"blink":true,"on_duration":0.5,"off_duration":0.5},"clip_rect_margin":3.0,"button_frame":true,"collapsing_header_frame":true,"indent_has_left_vline":true,"striped":true,"slider_trailing_fill":true,"handle_shape":{"Rect":{"aspect_ratio":0.5}},"interact_cursor":"Crosshair","image_loading_spinners":true,"numeric_color_space":"GammaByte","disabled_alpha":0.5},"animation_time":0.083333336,"debug":{"debug_on_hover":false,"warn_if_rect_changes_id":false, "show_focused_widget": false, "debug_on_hover_with_all_modifiers":false,"hover_shows_next":false,"show_expand_width":false,"show_expand_height":false,"show_resize":false,"show_interactive_widgets":false,"show_widget_hits":false,"show_unaligned":true},"explanation_tooltips":false,"url_in_tooltip":false,"always_scroll_the_only_direction":true,"scroll_animation":{"points_per_second":1000.0,"duration":{"min":0.1,"max":0.3}},"compact_menu_style":true}"#;

mod kimodo;
mod mcp;
mod plugins;

use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowResolution};
use jarvis_avatar::config::parse_present_mode;
use bevy_vrm1::prelude::*;
use jarvis_avatar::config::Settings;

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
    window.resolution = WindowResolution::new(
        settings.avatar.window_width,
        settings.avatar.window_height,
    );
    window.present_mode = parse_present_mode(&settings.graphics.present_mode);
}
