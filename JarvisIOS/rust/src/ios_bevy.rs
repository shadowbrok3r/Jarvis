//! Bevy 0.18 embedded in a UIKit `UIView` (Metal via wgpu), without `WinitPlugin`.
//! Injects `RawHandleWrapper` before `RenderPlugin` initializes so the swapchain is created.
//! Loads VRM / optional idle VRMA via `bevy_vrm1` (paths align with desktop `config/default.toml`).

use std::ffi::c_void;
use std::f32::consts::FRAC_PI_4;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use bevy::animation::RepeatAnimation;
use bevy::app::AnimationSystems;
use bevy::asset::{AssetMetaCheck, AssetPlugin};
use bevy::camera::{Exposure, PerspectiveProjection, Projection};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::MouseButton;
use bevy::input::touch::{TouchInput, TouchPhase};
use bevy::prelude::*;
use bevy::render::view::{Hdr, Msaa};
use bevy::render::RenderPlugin;
use bevy::window::{
    CursorOptions, ExitCondition, PrimaryWindow, RawHandleWrapper, RawHandleWrapperHolder,
    WindowPlugin, WindowWrapper,
};
use bevy::winit::WinitPlugin;
use bevy_egui::input::EguiWantsInput;
use bevy_egui::{EguiPlugin, EguiPostUpdateSet, EguiPrimaryContextPass};
use bevy_panorbit_camera::{ActiveCameraData, EguiFocusIncludesHover, PanOrbitCamera, PanOrbitCameraPlugin, TouchControls};
use bevy_vrm1::prelude::*;
use core::ptr::NonNull;

use crate::ios_graphics::{msaa_for_samples, IosGraphicsSettings};
use crate::ios_profile_manifest::{IosAvatarSettings, IosSpringPresetToml};
use crate::ios_spring_preset::{apply_spring_preset, parse_preset_toml};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, UiKitWindowHandle,
    WindowHandle,
};

// ── Asset root (Swift sets `JARVIS_ASSET_ROOT` to …/assets in the app resource bundle) ─────────

fn ios_asset_file_path() -> String {
    std::env::var("JARVIS_ASSET_ROOT").unwrap_or_else(|_| "assets".to_string())
}

/// Marks the scene root entity for the active VRM (hot-swap / diagnostics).
#[derive(Component, Debug, Clone, Copy, Default)]
pub(crate) struct JarvisIosAvatarRoot;

#[derive(Component)]
struct JarvisIosSun;

#[derive(Component)]
struct JarvisIosGroundPlane;

/// Entity with [`JarvisIosAvatarRoot`] (VRM + optional idle VRMA children).
#[derive(Resource, Default)]
struct IosAvatarRootEntity(Option<Entity>);

/// Passed into the app before `DefaultPlugins`; consumed by [`IosEmbedRawHandlesPlugin`].
///
/// Manual [`Resource`] impl avoids `#[derive(Resource)]` needing a direct `bevy_ecs` crate path
/// when Bevy is built with `default-features = false` for the iOS staticlib.
struct PendingIosSurface {
    view: NonNull<c_void>,
    width_px: u32,
    height_px: u32,
    scale_factor: f32,
}

// UIKit view pointer: only touched from the main thread via Swift `CADisplayLink` + FFI.
unsafe impl Send for PendingIosSurface {}
unsafe impl Sync for PendingIosSurface {}

impl Resource for PendingIosSurface {}

/// UIKit view pointer as a `HasWindowHandle` source for `RawHandleWrapper`.
#[derive(Clone)]
struct IosUiViewHost(NonNull<c_void>);

unsafe impl Send for IosUiViewHost {}
unsafe impl Sync for IosUiViewHost {}

impl HasWindowHandle for IosUiViewHost {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let ui = UiKitWindowHandle::new(self.0);
        let raw = RawWindowHandle::UiKit(ui);
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl HasDisplayHandle for IosUiViewHost {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::uikit())
    }
}

struct IosEmbedRawHandlesPlugin;

impl Plugin for IosEmbedRawHandlesPlugin {
    fn build(&self, app: &mut App) {
        let PendingIosSurface {
            view,
            width_px,
            height_px,
            scale_factor,
        } = app
            .world_mut()
            .remove_resource::<PendingIosSurface>()
            .expect("PendingIosSurface must be inserted before DefaultPlugins");

        let host = IosUiViewHost(view);
        let wrapped = WindowWrapper::new(host);
        let handle = RawHandleWrapper::new(&wrapped).expect("RawHandleWrapper::new for UIKit view");

        let entity = {
            let world = app.world_mut();
            let mut q = world.query_filtered::<(Entity, &mut Window, &RawHandleWrapperHolder), With<PrimaryWindow>>();
            let Ok((entity, mut window, holder)) = q.single_mut(world) else {
                panic!("JarvisIOS Bevy: missing PrimaryWindow entity");
            };

            window
                .resolution
                .set_physical_resolution(width_px.max(1), height_px.max(1));
            window.resolution.set_scale_factor(scale_factor);

            *holder.0.lock().expect("RawHandleWrapperHolder mutex") = Some(handle.clone());
            entity
        };

        app.world_mut().entity_mut(entity).insert(handle);
    }
}

// ── VRM scene (adapted from desktop `plugins/avatar.rs`) ───────────────────────────────────────

fn spawn_ios_viewport(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    settings: Res<IosAvatarSettings>,
    graphics: Res<IosGraphicsSettings>,
) {
    let focus = settings.world_position;
    let look_at = focus + Vec3::Y * 0.5;
    commands.spawn((
        JarvisIosSun,
        DirectionalLight {
            illuminance: graphics.directional_illuminance,
            shadows_enabled: graphics.directional_shadows,
            ..default()
        },
        Transform::from_translation(graphics.directional_position).looking_at(look_at, Vec3::Y),
    ));
    let half = (graphics.ground_size.max(0.02) * 0.5).min(512.0);
    let gc = graphics.ground_base_color;
    let ground_color = Color::linear_rgb(gc[0], gc[1], gc[2]);
    let ground_vis = if graphics.show_ground_plane {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    commands.spawn((
        JarvisIosGroundPlane,
        Mesh3d(meshes.add(Mesh::from(Plane3d::new(Vec3::Y, Vec2::splat(half))))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: ground_color,
            perceptual_roughness: 0.85,
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, 0.0),
        ground_vis,
    ));

    // Match desktop `OrbitCameraPlugin`: PanOrbit + touch (1-finger orbit, 2-finger pan, pinch zoom).
    // `bevy_panorbit_camera` reads `Touches`; we inject `TouchInput` from UIKit via `jarvis_renderer_touch`.
    let mut orbit = PanOrbitCamera::default();
    orbit.focus = focus;
    orbit.target_focus = focus;
    orbit.target_radius = 3.35;
    orbit.radius = Some(3.35);
    orbit.zoom_lower_limit = 0.35;
    orbit.zoom_upper_limit = Some(96.0);
    orbit.touch_enabled = true;
    orbit.touch_controls = TouchControls::OneFingerOrbit;
    orbit.button_orbit = MouseButton::Left;
    orbit.button_pan = MouseButton::Middle;
    orbit.button_zoom = None;
    let eye = focus + Vec3::new(0.0, 0.25, 3.35);

    let mut cam = commands.spawn((
        Camera3d::default(),
        msaa_for_samples(graphics.msaa_samples),
        Projection::Perspective(PerspectiveProjection {
            fov: FRAC_PI_4,
            near: 0.08,
            far: 200.0,
            ..default()
        }),
        Transform::from_translation(eye).looking_at(focus, Vec3::Y),
        orbit,
        // Match desktop `orbit_camera` + avoid default `TonyMcMapface` (LUT-heavy) on embedded Metal.
        Exposure {
            ev100: graphics.exposure_ev100,
        },
        Tonemapping::AcesFitted,
    ));
    if graphics.hdr {
        cam.insert(Hdr);
    }
}

/// Spawns [`JarvisIosAvatarRoot`] + [`VrmHandle`], optional idle VRMA; returns the root [`Entity`].
fn spawn_jarvis_ios_vrm_root(commands: &mut Commands, asset_server: &AssetServer, settings: &IosAvatarSettings) -> Entity {
    let asset_root = ios_asset_file_path();
    let vrm_disk = Path::new(&asset_root).join(&settings.model_path);
    crate::jarvis_ios_line!(
        "[JarvisIOS] spawn_jarvis_ios_vrm_root model_path={} exists_on_disk={} abs={}",
        settings.model_path,
        vrm_disk.is_file(),
        vrm_disk.display()
    );
    if !settings.idle_vrma_path.trim().is_empty() {
        let vrma_disk = Path::new(&asset_root).join(settings.idle_vrma_path.trim());
        crate::jarvis_ios_line!(
            "[JarvisIOS] spawn_jarvis_ios_vrm_root idle_vrma_path={} exists_on_disk={}",
            settings.idle_vrma_path,
            vrma_disk.is_file()
        );
    }

    let pos = settings.world_position;
    let scale = settings.uniform_scale.max(0.001);
    let mut vrm = commands.spawn((
        JarvisIosAvatarRoot,
        Transform {
            translation: pos,
            scale: Vec3::splat(scale),
            ..default()
        },
        GlobalTransform::default(),
        VrmHandle(asset_server.load(settings.model_path.clone())),
    ));
    crate::jarvis_ios_line!(
        "[JarvisIOS] spawn_jarvis_ios_vrm_root queued VrmHandle for {}",
        settings.model_path
    );

    if !settings.idle_vrma_path.trim().is_empty() {
        let path = settings.idle_vrma_path.clone();
        vrm.with_children(|parent| {
            parent
                .spawn(VrmaHandle(asset_server.load(path)))
                .observe(play_idle_when_vrma_loaded);
        });
    }
    vrm.id()
}

fn spawn_ios_avatar(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    settings: Res<IosAvatarSettings>,
) {
    commands.insert_resource(ClearColor(settings.background_color));
    let id = spawn_jarvis_ios_vrm_root(&mut commands, &asset_server, &settings);
    commands.insert_resource(IosAvatarRootEntity(Some(id)));
}

fn play_idle_when_vrma_loaded(trigger: On<LoadedVrma>, mut commands: Commands) {
    commands.trigger(PlayVrma {
        repeat: RepeatAnimation::Forever,
        transition_duration: Duration::ZERO,
        vrma: trigger.vrma,
        reset_spring_bones: false,
    });
}

fn observe_vrma_play_forever(trigger: On<LoadedVrma>, mut commands: Commands) {
    commands.trigger(PlayVrma {
        repeat: RepeatAnimation::Forever,
        transition_duration: Duration::from_millis(300),
        vrma: trigger.vrma,
        reset_spring_bones: false,
    });
}

fn observe_vrma_play_once(trigger: On<LoadedVrma>, mut commands: Commands) {
    commands.trigger(PlayVrma {
        repeat: RepeatAnimation::Never,
        transition_duration: Duration::from_millis(300),
        vrma: trigger.vrma,
        reset_spring_bones: false,
    });
}

pub(crate) fn is_safe_asset_rel(rel: &str) -> bool {
    !rel.is_empty() && !rel.starts_with('/') && !rel.contains("..")
}

fn apply_scene_graphics_from_settings(world: &mut World, g: &IosGraphicsSettings, focus: Vec3) {
    let look_at = focus + Vec3::Y * 0.5;
    let cam_entities: Vec<Entity> = world
        .query_filtered::<Entity, With<Camera3d>>()
        .iter(world)
        .collect();
    for e in cam_entities {
        let mut ew = world.entity_mut(e);
        if let Some(mut m) = ew.get_mut::<Msaa>() {
            *m = msaa_for_samples(g.msaa_samples);
        }
        if let Some(mut exp) = ew.get_mut::<Exposure>() {
            exp.ev100 = g.exposure_ev100;
        }
        if g.hdr {
            ew.insert(Hdr);
        } else {
            ew.remove::<Hdr>();
        }
    }
    let mut sun_q = world.query_filtered::<(&mut DirectionalLight, &mut Transform), With<JarvisIosSun>>();
    for (mut dl, mut tf) in sun_q.iter_mut(world) {
        dl.illuminance = g.directional_illuminance;
        dl.shadows_enabled = g.directional_shadows;
        *tf = Transform::from_translation(g.directional_position).looking_at(look_at, Vec3::Y);
    }
    let half = (g.ground_size.max(0.02) * 0.5).min(512.0);
    let gc = g.ground_base_color;
    let ground_color = Color::linear_rgb(gc[0], gc[1], gc[2]);
    let ground_mesh_handles: Vec<Handle<Mesh>> = world
        .query_filtered::<&Mesh3d, With<JarvisIosGroundPlane>>()
        .iter(world)
        .map(|m| m.0.clone())
        .collect();
    {
        let mut meshes = world.resource_mut::<Assets<Mesh>>();
        for h in &ground_mesh_handles {
            if let Some(m) = meshes.get_mut(h) {
                *m = Mesh::from(Plane3d::new(Vec3::Y, Vec2::splat(half)));
            }
        }
    }
    for mut vis in world
        .query_filtered::<&mut Visibility, With<JarvisIosGroundPlane>>()
        .iter_mut(world)
    {
        *vis = if g.show_ground_plane {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let handles: Vec<Handle<StandardMaterial>> = world
        .query_filtered::<&MeshMaterial3d<StandardMaterial>, With<JarvisIosGroundPlane>>()
        .iter(world)
        .map(|m| m.0.clone())
        .collect();
    let mut mats = world.resource_mut::<Assets<StandardMaterial>>();
    for h in handles {
        if let Some(m) = mats.get_mut(&h) {
            m.base_color = ground_color;
        }
    }
}

fn update_panorbit_camera_focus(world: &mut World, focus: Vec3) {
    let mut q = world.query_filtered::<&mut PanOrbitCamera, With<Camera3d>>();
    for mut orbit in q.iter_mut(world) {
        orbit.focus = focus;
        orbit.target_focus = focus;
    }
}

fn ios_apply_spring_preset_on_vrm_ready(
    preset: Res<IosSpringPresetToml>,
    vrm_ready: Query<(), (With<Vrm>, Added<Initialized>)>,
    mut springs: Query<(Entity, Option<&Name>, &mut SpringJointProps)>,
    mut colliders: Query<(Entity, Option<&Name>, &mut ColliderShape)>,
) {
    if preset.0.is_none() || vrm_ready.is_empty() {
        return;
    }
    let Some(raw) = preset.0.as_deref() else {
        return;
    };
    let Ok(p) = parse_preset_toml(raw) else {
        crate::jarvis_ios_line!("[JarvisIOS] spring preset: TOML parse failed");
        return;
    };
    if p.preset_version != crate::ios_spring_preset::PRESET_FORMAT_VERSION {
        crate::jarvis_ios_line!(
            "[JarvisIOS] spring preset: unexpected preset_version {} (expected {})",
            p.preset_version,
            crate::ios_spring_preset::PRESET_FORMAT_VERSION
        );
    }
    let (jh, jm, ch, cm) = apply_spring_preset(&p, &mut springs, &mut colliders);
    crate::jarvis_ios_line!(
        "[JarvisIOS] spring preset applied: joints {}/{} colliders {}/{}",
        jh,
        jh + jm,
        ch,
        ch + cm
    );
}

fn lock_hips_root_motion(
    settings: Res<IosAvatarSettings>,
    mut hips_q: Query<(&mut Transform, &RestTransform), With<Hips>>,
) {
    if !settings.lock_root_xz && !settings.lock_root_y {
        return;
    }
    for (mut tf, rest) in &mut hips_q {
        let r = rest.0.translation;
        if settings.lock_root_xz {
            tf.translation.x = r.x;
            tf.translation.z = r.z;
        }
        if settings.lock_root_y {
            tf.translation.y = r.y;
        }
    }
}

/// After a few frames, log whether any `Vrm` entity exists (async load can delay spawn).
///
/// Embedded UIKit: `active_viewport_data` often never picks our camera, so we pin PanOrbit to the
/// primary 3D camera with [`ActiveCameraData::manual`]. **Critical:** we must clear
/// [`ActiveCameraData::entity`] whenever egui wants the pointer — [`absorb_bevy_input_system`] does
/// not clear Bevy [`Touches`], so one-finger orbit would otherwise steal drags from egui windows.
///
/// Runs in [`PostUpdate`] **after** [`EguiPostUpdateSet::ProcessOutput`] so [`EguiWantsInput`] reflects
/// the frame's egui pass, and **before** [`PanOrbitCameraSystemSet`] so PanOrbit sees a cleared camera.
fn sync_ios_panorbit_active_camera(
    mut active: ResMut<ActiveCameraData>,
    mut warned: Local<bool>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cam_q: Query<Entity, (With<Camera3d>, With<PanOrbitCamera>)>,
    egui_wants: Res<EguiWantsInput>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let cams: Vec<Entity> = cam_q.iter().collect();
    let entity = match cams.as_slice() {
        [e] => {
            *warned = false;
            *e
        }
        [] => {
            if !*warned {
                crate::jarvis_ios_line!(
                    "[JarvisIOS] panorbit sync: no Camera3d+PanOrbit entity (viewport may stay black)"
                );
                *warned = true;
            }
            return;
        }
        many => {
            if !*warned {
                crate::jarvis_ios_line!(
                    "[JarvisIOS] panorbit sync: {} Camera3d+PanOrbit entities (expected 1); using first",
                    many.len()
                );
                *warned = true;
            }
            many[0]
        }
    };
    let logical = Vec2::new(window.width(), window.height());
    if logical.x <= 1e-3 || logical.y <= 1e-3 {
        return;
    }
    let cam_entity = if egui_wants.wants_any_pointer_input() {
        None
    } else {
        Some(entity)
    };
    active.set_if_neq(ActiveCameraData {
        entity: cam_entity,
        viewport_size: Some(logical),
        window_size: Some(logical),
        manual: true,
    });
}

fn jarvis_ios_vrm_load_diag(mut frames: Local<u32>, vrm_q: Query<(), With<Vrm>>) {
    *frames += 1;
    if *frames == 30 || *frames == 120 {
        let n = vrm_q.iter().count();
        crate::jarvis_ios_line!(
            "[JarvisIOS] diag: update_frame={} entities_with_Vrm={}",
            *frames,
            n
        );
    }
}

fn clamp_vrm_root_y(settings: Res<IosAvatarSettings>, mut vrm_q: Query<&mut Transform, With<Vrm>>) {
    if !settings.lock_vrm_root_y {
        return;
    }
    let target_y = settings.world_position.y;
    for mut tf in &mut vrm_q {
        if (tf.translation.y - target_y).abs() > f32::EPSILON {
            tf.translation.y = target_y;
        }
    }
}

pub struct IosEmbeddedRenderer {
    app: App,
    /// Log render enter/leave for the first N frames only (diagnostics; avoid log spam).
    render_diag_frames_to_log: u8,
    /// After a panic in `app.update()`, Bevy state is undefined; skip further ticks (FFI must not unwind).
    render_poisoned: AtomicBool,
    /// Primary window entity for injected [`TouchInput`] messages.
    primary_window: Entity,
    /// UIKit touches forwarded from Swift; drained at the start of each `render()` / `app.update()`.
    touch_queue: Mutex<Vec<(u8, f32, f32, u64)>>,
    /// Re-read manifest + swap VRM (set from Swift after hub sync).
    profile_reload_pending: Mutex<bool>,
    /// VRMA paths relative to `JARVIS_ASSET_ROOT` (asset server root).
    vrma_play_queue: Mutex<Vec<(String, bool)>>,
    /// Pose-library JSON paths relative to `JARVIS_ASSET_ROOT` (last queued wins).
    json_anim_queue: Mutex<Vec<String>>,
}

impl IosEmbeddedRenderer {
    pub fn new(ui_view: *mut c_void, width_px: u32, height_px: u32, pixels_per_point: f32) -> Option<Self> {
        let view = NonNull::new(ui_view)?;
        let scale = pixels_per_point.max(0.5);

        let mut primary_window = Window::default();
        primary_window
            .resolution
            .set_physical_resolution(width_px.max(1), height_px.max(1));
        primary_window.resolution.set_scale_factor(scale);

        let asset_file_path = ios_asset_file_path();
        info!("JarvisIOS Bevy asset root (file_path): {asset_file_path}");
        crate::jarvis_ios_line!("[JarvisIOS] Bevy AssetPlugin file_path={asset_file_path}");
        if let Ok(p) = std::env::var("JARVIS_PROFILE_MANIFEST") {
            info!("JarvisIOS profile manifest: {p}");
            crate::jarvis_ios_line!("[JarvisIOS] JARVIS_PROFILE_MANIFEST={p}");
        } else {
            crate::jarvis_ios_line!("[JarvisIOS] JARVIS_PROFILE_MANIFEST unset");
        }

        let mut app = App::new();
        app.insert_resource(PendingIosSurface {
            view,
            width_px: width_px.max(1),
            height_px: height_px.max(1),
            scale_factor: scale,
        });
        let (avatar_settings, graphics_settings, spring_toml) =
            crate::ios_profile_manifest::load_ios_hub_profile_bundle_from_env();
        crate::jarvis_ios_line!(
            "[JarvisIOS] IosAvatarSettings model_path={} idle_vrma_path={}",
            avatar_settings.model_path,
            avatar_settings.idle_vrma_path
        );
        app.insert_resource(avatar_settings);
        app.insert_resource(graphics_settings);
        app.insert_resource(IosSpringPresetToml(spring_toml));
        app.init_resource::<IosAvatarRootEntity>();

        app.add_plugins(
            DefaultPlugins
                .build()
                .set(AssetPlugin {
                    file_path: asset_file_path,
                    meta_check: AssetMetaCheck::Never,
                    ..default()
                })
                .disable::<WinitPlugin>()
                .set(WindowPlugin {
                    primary_window: Some(primary_window),
                    primary_cursor_options: Some(CursorOptions::default()),
                    exit_condition: ExitCondition::DontExit,
                    close_when_requested: false,
                })
                .add_before::<RenderPlugin>(IosEmbedRawHandlesPlugin),
        );
        app.add_plugins((VrmPlugin, VrmaPlugin, EguiPlugin::default(), PanOrbitCameraPlugin));
        crate::ios_anim_json::plugin(&mut app);
        // When the pointer is over egui (menu bar, windows), PanOrbit must not consume drags/pinch.
        let mut egui_global = bevy_egui::EguiGlobalSettings::default();
        egui_global.enable_absorb_bevy_input_system = true;
        app.insert_resource(egui_global);
        app.insert_resource(EguiFocusIncludesHover(true));
        app.init_resource::<crate::ios_egui_ui::JarvisIosUiState>();

        app.add_systems(
            Startup,
            (spawn_ios_viewport, spawn_ios_avatar).chain(),
        );
        app.add_systems(
            PostUpdate,
            (lock_hips_root_motion, clamp_vrm_root_y)
                .chain()
                .after(AnimationSystems),
        );
        app.add_systems(
            PostUpdate,
            sync_ios_panorbit_active_camera
                .after(EguiPostUpdateSet::ProcessOutput)
                .before(bevy_panorbit_camera::PanOrbitCameraSystemSet),
        );
        app.add_systems(
            Update,
            (ios_apply_spring_preset_on_vrm_ready, jarvis_ios_vrm_load_diag),
        );
        app.add_systems(
            EguiPrimaryContextPass,
            (
                crate::ios_egui_ui::jarvis_ios_egui_apply_theme,
                crate::ios_egui_ui::jarvis_ios_egui_menu_bar,
                crate::ios_egui_ui::jarvis_ios_egui_windows,
            )
                .chain(),
        );

        // Without `WinitPlugin`, nothing runs Bevy's default `run_once` runner, which waits for
        // `PluginsState::Adding`, then calls `App::finish` / `cleanup` before the first `update()`.
        // `RenderPlugin::finish` inserts `RenderDevice` (and clones) on the **main** world; until
        // then, `bevy_pbr`'s `PostUpdate` systems like `no_automatic_skin_batching` see a missing
        // `Res<RenderDevice>` and fail strict validation (panic in debug builds).
        while app.plugins_state() == bevy::app::PluginsState::Adding {
            // iOS-only module: always tick pools (mirrors `bevy_app::run_once`); use `bevy::tasks`
            // so we do not need a direct `bevy_tasks` dependency.
            bevy::tasks::tick_global_task_pools_on_main_thread();
        }
        app.finish();
        app.cleanup();

        let primary_window = {
            let world = app.world_mut();
            let mut q = world.query_filtered::<Entity, With<PrimaryWindow>>();
            q.iter(world)
                .next()
                .expect("JarvisIOS: PrimaryWindow missing after Bevy init")
        };

        Some(Self {
            app,
            render_diag_frames_to_log: 5,
            render_poisoned: AtomicBool::new(false),
            primary_window,
            touch_queue: Mutex::new(Vec::new()),
            profile_reload_pending: Mutex::new(false),
            vrma_play_queue: Mutex::new(Vec::new()),
            json_anim_queue: Mutex::new(Vec::new()),
        })
    }

    pub fn note_render_poisoned(&self) {
        self.render_poisoned.store(true, Ordering::Release);
    }

    pub fn queue_touch(&self, phase: u8, x: f32, y: f32, id: u64) {
        if let Ok(mut g) = self.touch_queue.lock() {
            g.push((phase, x, y, id));
        }
    }

    pub fn queue_profile_reload(&self) {
        if let Ok(mut g) = self.profile_reload_pending.lock() {
            *g = true;
        }
    }

    pub fn queue_vrma_play(&self, path: String, loop_forever: bool) {
        if let Ok(mut g) = self.vrma_play_queue.lock() {
            g.push((path, loop_forever));
        }
    }

    pub fn queue_json_anim_play(&self, path: String) {
        if let Ok(mut g) = self.json_anim_queue.lock() {
            g.push(path);
        }
    }

    fn flush_queued_json_anim_requests(&mut self) {
        let drained: Vec<String> = {
            let mut g = self.json_anim_queue.lock().unwrap();
            core::mem::take(&mut *g)
        };
        if drained.is_empty() {
            return;
        }
        let path = drained.into_iter().last().unwrap();
        let world = self.app.world_mut();
        let clip = crate::ios_anim_json::try_build_clip(&path, world);
        world
            .resource_mut::<crate::ios_anim_json::IosJsonAnimPlayback>()
            .replace_with_clip(clip);
    }

    fn flush_queued_vrma_requests(&mut self) {
        let drained: Vec<_> = {
            let mut g = self.vrma_play_queue.lock().unwrap();
            core::mem::take(&mut *g)
        };
        if drained.is_empty() {
            return;
        }
        let world = self.app.world_mut();
        let Some(root) = world.resource::<IosAvatarRootEntity>().0 else {
            crate::jarvis_ios_line!("[JarvisIOS] queue_vrma: no avatar root (reload profile first)");
            return;
        };
        let asset_server = world.resource::<AssetServer>().clone();
        for (path, loop_forever) in drained {
            if !is_safe_asset_rel(&path) {
                crate::jarvis_ios_line!("[JarvisIOS] queue_vrma: rejected unsafe path {path:?}");
                continue;
            }
            if loop_forever {
                world.commands().entity(root).with_children(|parent| {
                    parent
                        .spawn(VrmaHandle(asset_server.load(path.clone())))
                        .observe(observe_vrma_play_forever);
                });
            } else {
                world.commands().entity(root).with_children(|parent| {
                    parent
                        .spawn(VrmaHandle(asset_server.load(path.clone())))
                        .observe(observe_vrma_play_once);
                });
            }
            crate::jarvis_ios_line!("[JarvisIOS] queue_vrma: spawned {path} loop={loop_forever}");
        }
        world.flush();
    }

    fn apply_hub_profile_reload(&mut self) {
        let (avatar, graphics, spring) = crate::ios_profile_manifest::load_ios_hub_profile_bundle_from_env();
        let world = self.app.world_mut();
        *world.resource_mut::<IosAvatarSettings>() = avatar.clone();
        *world.resource_mut::<IosGraphicsSettings>() = graphics.clone();
        *world.resource_mut::<IosSpringPresetToml>() = IosSpringPresetToml(spring);
        world.insert_resource(ClearColor(avatar.background_color));
        // Despawn every avatar root (VRM + VRMA children). Relying on a single stored entity can miss
        // duplicates if a previous reload partially failed, which breaks PanOrbit + leaves a black view.
        let roots: Vec<Entity> = world
            .query_filtered::<Entity, With<JarvisIosAvatarRoot>>()
            .iter(world)
            .collect();
        for e in roots {
            world.entity_mut(e).despawn();
        }
        world.flush();
        world.insert_resource(IosAvatarRootEntity(None));
        apply_scene_graphics_from_settings(world, &graphics, avatar.world_position);
        update_panorbit_camera_focus(world, avatar.world_position);
        let asset_server = world.resource::<AssetServer>().clone();
        let id = spawn_jarvis_ios_vrm_root(&mut world.commands(), &asset_server, &avatar);
        world.insert_resource(IosAvatarRootEntity(Some(id)));
        world.flush();
        world
            .resource_mut::<crate::ios_anim_json::IosJsonAnimPlayback>()
            .replace_with_clip(None);
        crate::jarvis_ios_line!(
            "[JarvisIOS] profile reload applied model_path={} msaa_samples={}",
            avatar.model_path,
            graphics.msaa_samples
        );
    }

    fn flush_queued_touch_inputs(&mut self) {
        let drained: Vec<_> = {
            let mut g = self.touch_queue.lock().unwrap();
            core::mem::take(&mut *g)
        };
        if drained.is_empty() {
            return;
        }
        let win = self.primary_window;
        let world = self.app.world_mut();
        for (phase, x, y, id) in drained {
            let phase = match phase {
                0 => TouchPhase::Started,
                1 => TouchPhase::Moved,
                2 => TouchPhase::Ended,
                _ => TouchPhase::Canceled,
            };
            world.write_message(TouchInput {
                phase,
                position: Vec2::new(x, y),
                window: win,
                force: None,
                id,
            });
        }
    }

    pub fn render(&mut self) {
        if self.render_poisoned.load(Ordering::Acquire) {
            return;
        }
        if self.render_diag_frames_to_log > 0 {
            crate::jarvis_ios_line!("[JarvisIOS] render: app.update() enter");
        }
        let reload = match self.profile_reload_pending.lock() {
            Ok(mut g) => core::mem::take(&mut *g),
            Err(_) => false,
        };
        if reload {
            self.apply_hub_profile_reload();
        }
        self.flush_queued_json_anim_requests();
        self.flush_queued_vrma_requests();
        self.flush_queued_touch_inputs();
        self.app.update();
        if self.render_diag_frames_to_log > 0 {
            crate::jarvis_ios_line!("[JarvisIOS] render: app.update() leave");
            self.render_diag_frames_to_log -= 1;
        }
    }

    pub fn resize(&mut self, width_px: u32, height_px: u32) {
        let world = self.app.world_mut();
        let mut q = world.query_filtered::<&mut Window, With<PrimaryWindow>>();
        let Ok(mut window) = q.single_mut(world) else {
            return;
        };
        window
            .resolution
            .set_physical_resolution(width_px.max(1), height_px.max(1));
    }
}
