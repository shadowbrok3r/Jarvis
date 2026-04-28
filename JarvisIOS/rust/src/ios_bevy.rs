//! Bevy 0.18 embedded in a UIKit `UIView` (Metal via wgpu), without `WinitPlugin`.
//! Injects `RawHandleWrapper` before `RenderPlugin` initializes so the swapchain is created.
//! Loads VRM / optional idle VRMA via `bevy_vrm1` (paths align with desktop `config/default.toml`).

use std::ffi::c_void;
use std::path::Path;
use std::time::Duration;

use bevy::animation::RepeatAnimation;
use bevy::app::AnimationSystems;
use bevy::asset::{AssetMetaCheck, AssetPlugin};
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::window::{
    CursorOptions, ExitCondition, PrimaryWindow, RawHandleWrapper, RawHandleWrapperHolder,
    WindowPlugin, WindowWrapper,
};
use bevy::winit::WinitPlugin;
use bevy_vrm1::prelude::*;
use core::ptr::NonNull;

use crate::ios_profile_manifest::IosAvatarSettings;
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
struct JarvisIosAvatarRoot;

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
) {
    commands.spawn((
        DirectionalLight {
            illuminance: light_consts::lux::OVERCAST_DAY,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 8.0, 4.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
    ));
    // Ground plane (optional context)
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(12.0)))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.12, 0.12, 0.14),
            perceptual_roughness: 0.85,
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, 0.0),
    ));
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 1.45, 3.35).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
    ));
}

fn spawn_ios_avatar(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    settings: Res<IosAvatarSettings>,
) {
    let asset_root = ios_asset_file_path();
    let vrm_disk = Path::new(&asset_root).join(&settings.model_path);
    crate::jarvis_ios_line!(
        "[JarvisIOS] spawn_ios_avatar model_path={} exists_on_disk={} abs={}",
        settings.model_path,
        vrm_disk.is_file(),
        vrm_disk.display()
    );
    if !settings.idle_vrma_path.trim().is_empty() {
        let vrma_disk = Path::new(&asset_root).join(settings.idle_vrma_path.trim());
        crate::jarvis_ios_line!(
            "[JarvisIOS] spawn_ios_avatar idle_vrma_path={} exists_on_disk={}",
            settings.idle_vrma_path,
            vrma_disk.is_file()
        );
    }

    commands.insert_resource(ClearColor(settings.background_color));
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
        "[JarvisIOS] spawn_ios_avatar queued VrmHandle for {}",
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
}

fn play_idle_when_vrma_loaded(trigger: On<LoadedVrma>, mut commands: Commands) {
    commands.trigger(PlayVrma {
        repeat: RepeatAnimation::Forever,
        transition_duration: Duration::ZERO,
        vrma: trigger.vrma,
        reset_spring_bones: false,
    });
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
        let avatar_settings = IosAvatarSettings::from_env_manifest_or_default();
        crate::jarvis_ios_line!(
            "[JarvisIOS] IosAvatarSettings model_path={} idle_vrma_path={}",
            avatar_settings.model_path,
            avatar_settings.idle_vrma_path
        );
        app.insert_resource(avatar_settings);

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
        app.add_plugins((VrmPlugin, VrmaPlugin));

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
        app.add_systems(Update, jarvis_ios_vrm_load_diag);

        Some(Self { app })
    }

    pub fn render(&mut self) {
        self.app.update();
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
