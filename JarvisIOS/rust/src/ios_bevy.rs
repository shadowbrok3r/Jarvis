//! Bevy 0.18 embedded in a UIKit `UIView` (Metal via wgpu), without `WinitPlugin`.
//! Injects `RawHandleWrapper` before `RenderPlugin` initializes so the swapchain is created.

use std::ffi::c_void;

use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::window::{
    CursorOptions, ExitCondition, RawHandleWrapper, RawHandleWrapperHolder, WindowPlugin,
    WindowWrapper,
};
use bevy::winit::WinitPlugin;
use core::ptr::NonNull;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, UiKitWindowHandle,
    WindowHandle,
};

/// Passed into the app before `DefaultPlugins`; consumed by [`IosEmbedRawHandlesPlugin`].
#[derive(Resource)]
struct PendingIosSurface {
    view: NonNull<c_void>,
    width_px: u32,
    height_px: u32,
    scale_factor: f32,
}

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
            let Ok((entity, mut window, holder)) = q.get_single_mut(world) else {
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

fn setup_demo_scene(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(1.2, 1.2, 1.2))),
        MeshMaterial3d(materials.add(Color::srgb(0.85, 0.45, 0.35))),
        Transform::from_xyz(0.0, 0.6, 0.0),
    ));
    commands.spawn((
        PointLight {
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 6.0, 4.0),
    ));
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(-2.8, 2.2, 6.4).looking_at(Vec3::new(0.0, 0.4, 0.0), Vec3::Y),
    ));
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

        let mut app = App::new();
        app.insert_resource(PendingIosSurface {
            view,
            width_px: width_px.max(1),
            height_px: height_px.max(1),
            scale_factor: scale,
        });

        app.add_plugins(
            DefaultPlugins
                .build()
                .disable::<WinitPlugin>()
                .set(WindowPlugin {
                    primary_window: Some(primary_window),
                    primary_cursor_options: Some(CursorOptions::default()),
                    exit_condition: ExitCondition::DontExit,
                    close_when_requested: false,
                })
                .add_before::<RenderPlugin, IosEmbedRawHandlesPlugin>(IosEmbedRawHandlesPlugin),
        );
        app.add_systems(Startup, setup_demo_scene);

        Some(Self { app })
    }

    pub fn render(&mut self) {
        self.app.update();
    }

    pub fn resize(&mut self, width_px: u32, height_px: u32) {
        let world = self.app.world_mut();
        let mut q = world.query_filtered::<&mut Window, With<PrimaryWindow>>();
        let Ok(mut window) = q.get_single_mut(world) else {
            return;
        };
        window
            .resolution
            .set_physical_resolution(width_px.max(1), height_px.max(1));
    }
}
