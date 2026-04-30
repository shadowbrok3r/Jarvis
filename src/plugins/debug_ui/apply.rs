//! Systems that reactively push `Settings` field changes into Bevy
//! resources/components on the same frame. Each guards on `settings.is_changed()`
//! so they stay free when the UI is idle.

use bevy::camera::Exposure;
use bevy::light::GlobalAmbientLight;
use bevy::prelude::*;
use bevy::render::view::Msaa;
use bevy::window::PrimaryWindow;
use bevy_panorbit_camera::PanOrbitCamera;
use bevy_vrm1::prelude::Vrm;

use jarvis_avatar::config::{Settings, msaa_from_settings, parse_present_mode};

use super::DebugUiState;
use crate::plugins::environment::{GroundPlane, SunLight};

pub fn apply_camera_settings(
    settings: Res<Settings>,
    mut state: ResMut<DebugUiState>,
    mut orbit_q: Query<&mut PanOrbitCamera>,
    vrm_tf: Query<&GlobalTransform, With<Vrm>>,
) {
    let cam = &settings.camera;
    for mut orbit in &mut orbit_q {
        orbit.zoom_lower_limit = cam.min_radius;
        orbit.zoom_upper_limit = Some(cam.max_radius);
        orbit.orbit_sensitivity = cam.orbit_sensitivity;
        orbit.pan_sensitivity = cam.pan_sensitivity;
        orbit.zoom_sensitivity = cam.zoom_sensitivity;
        orbit.orbit_smoothness = cam.orbit_smoothness;
        orbit.zoom_smoothness = cam.zoom_smoothness;
        orbit.pan_smoothness = cam.pan_smoothness;

        if state.resnap_requested {
            if let Ok(vrm_gtf) = vrm_tf.single() {
                let target = vrm_gtf.translation() + Vec3::Y * cam.focus_y_lift;
                orbit.focus = target;
                orbit.target_focus = target;
                orbit.target_radius = cam.initial_radius;
                orbit.radius = Some(cam.initial_radius);
                orbit.force_update = true;
            }
        }
    }
    if state.resnap_requested {
        state.resnap_requested = false;
    }
}

pub fn apply_avatar_transform(
    settings: Res<Settings>,
    mut vrm_q: Query<&mut Transform, With<Vrm>>,
) {
    if !settings.is_changed() {
        return;
    }
    let target = Vec3::from_array(settings.avatar.world_position);
    let scale = settings.avatar.uniform_scale.max(0.001);
    for mut tf in &mut vrm_q {
        tf.translation = target;
        tf.scale = Vec3::splat(scale);
    }
}

/// Pushes `Settings::graphics.msaa_samples` onto every [`Camera3d`]'s [`Msaa`]
/// component so MSAA changes apply without restarting the app.
pub fn sync_camera_msaa(settings: Res<Settings>, mut msaa_q: Query<&mut Msaa, With<Camera3d>>) {
    if !settings.is_changed() {
        return;
    }
    let m = msaa_from_settings(settings.graphics.msaa_samples);
    for mut comp in &mut msaa_q {
        *comp = m;
    }
}

/// Keeps the OS swapchain in sync with `Settings::graphics.present_mode` (VSync policy).
pub fn apply_window_present_mode(
    settings: Res<Settings>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if !settings.is_changed() {
        return;
    }
    let mode = parse_present_mode(&settings.graphics.present_mode);
    for mut w in &mut windows {
        w.present_mode = mode;
    }
}

pub fn apply_clear_color(settings: Res<Settings>, mut clear: ResMut<ClearColor>) {
    if !settings.is_changed() {
        return;
    }
    let [r, g, b, a] = settings.avatar.background_color;
    clear.0 = Color::linear_rgba(r, g, b, a);
}

pub fn apply_ambient_light(settings: Res<Settings>, mut ambient: ResMut<GlobalAmbientLight>) {
    if !settings.is_changed() {
        return;
    }
    let [r, g, b, a] = settings.graphics.ambient_color;
    ambient.color = Color::linear_rgba(r, g, b, a);
    ambient.brightness = settings.graphics.ambient_brightness;
}

pub fn apply_exposure(settings: Res<Settings>, mut cam_q: Query<&mut Exposure>) {
    if !settings.is_changed() {
        return;
    }
    for mut e in &mut cam_q {
        e.ev100 = settings.graphics.exposure_ev100;
    }
}

pub fn apply_sun_light(
    settings: Res<Settings>,
    mut sun_q: Query<(&mut DirectionalLight, &mut Transform), With<SunLight>>,
) {
    if !settings.is_changed() {
        return;
    }
    // When the three-point rig is running it owns the SunLight (key light).
    // Letting both systems write stomps the key light every frame.
    if settings.light_rig.enabled {
        return;
    }
    let g = &settings.graphics;
    for (mut dl, mut tf) in &mut sun_q {
        dl.illuminance = g.directional_illuminance;
        dl.shadows_enabled = g.directional_shadows;
        let pos = Vec3::from_array(g.directional_position);
        let look = Vec3::from_array(g.directional_look_at);
        *tf = Transform::from_translation(pos).looking_at(look, Vec3::Y);
    }
}

pub fn apply_ground_material(
    settings: Res<Settings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut ground_q: Query<(&GroundPlane, &mut Visibility)>,
) {
    if !settings.is_changed() {
        return;
    }
    let g = &settings.graphics;
    let [r, gc, b] = g.ground_base_color;
    for (plane, mut vis) in &mut ground_q {
        if let Some(mat) = materials.get_mut(&plane.material) {
            mat.base_color = Color::linear_rgb(r, gc, b);
        }
        if let Some(mesh) = meshes.get_mut(&plane.mesh) {
            let half = g.ground_size * 0.5;
            *mesh = Plane3d::new(Vec3::Y, Vec2::new(half, half)).into();
        }
        *vis = if g.show_ground_plane {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}
