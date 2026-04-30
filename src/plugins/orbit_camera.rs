//! Orbit (LMB), pan (MMB), zoom (scroll) via `bevy_panorbit_camera`.
//!
//! The plugin adds two behaviors on top of the bare `PanOrbitCameraPlugin`:
//!
//! 1. **Focus-follow-VRM** — the first time the `Vrm` root is located the
//!    orbit focus snaps onto it, and after that every frame (before the
//!    panorbit plugin integrates input) the focus is re-pinned so the camera
//!    doesn't drift if the rig moves a few cm per frame during an animation.
//!
//! 2. **Recenter on orbit/zoom** — panning is allowed to drag the focus off
//!    the VRM so the user can push her into the left/right/top/bottom of the
//!    viewport for framing, but the instant they start a new orbit (LMB) or
//!    zoom (scroll) input we snap focus *back* onto the VRM so rotation and
//!    zoom always pivot around her. This preserves panned framing between
//!    interactions without losing the "she's always the center of attention"
//!    invariant the rest of the UX assumes.
//!
//! All of this is gated on `Settings::camera.focus_follow_vrm` /
//! `recenter_on_orbit_zoom`; turning them off restores stock PanOrbitCamera
//! behavior.

use bevy::camera::{Exposure, PerspectiveProjection, Projection};
use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy::render::view::Hdr;
use bevy_egui::EguiContexts;
use bevy_panorbit_camera::{PanOrbitCamera, PanOrbitCameraPlugin, PanOrbitCameraSystemSet};
use bevy_vrm1::prelude::Vrm;

use jarvis_avatar::config::{Settings, msaa_from_settings};

use crate::plugins::rig_editor::RigEditorState;

pub struct OrbitCameraPlugin;

impl Plugin for OrbitCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VrmFocusSnapState>()
            .init_resource::<RigTwistOrbitGate>()
            .add_plugins(PanOrbitCameraPlugin)
            .add_systems(Startup, spawn_orbit_camera)
            .add_systems(
                PostUpdate,
                rig_editor_suppress_orbit_for_twist.before(PanOrbitCameraSystemSet),
            )
            .add_systems(
                Update,
                (
                    recenter_on_orbit_zoom_input,
                    snap_orbit_focus_to_vrm_root,
                    apply_projection_settings,
                ),
            );
    }
}

/// While the rig editor handles Alt+LMB twist, we temporarily clear
/// [`PanOrbitCamera::enabled`] and restore the prior flag afterward.
#[derive(Resource, Default)]
struct RigTwistOrbitGate {
    saved_enabled: Option<bool>,
}

fn rig_editor_suppress_orbit_for_twist(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    rig: Res<RigEditorState>,
    mut gate: ResMut<RigTwistOrbitGate>,
    mut orbit_q: Query<&mut PanOrbitCamera, With<Camera3d>>,
) {
    let egui_blocks = match contexts.ctx_mut() {
        Ok(ctx) => ctx.wants_pointer_input(),
        Err(_) => false,
    };

    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let twist_viewport = rig.twist_drag_enabled
        && rig.selected_bone.is_some()
        && alt
        && mouse.pressed(MouseButton::Left)
        && !egui_blocks;

    for mut orbit in &mut orbit_q {
        if twist_viewport {
            if gate.saved_enabled.is_none() {
                gate.saved_enabled = Some(orbit.enabled);
            }
            orbit.enabled = false;
        } else if let Some(prev) = gate.saved_enabled.take() {
            orbit.enabled = prev;
        }
    }
}

#[derive(Resource, Default)]
struct VrmFocusSnapState {
    /// Consecutive frames the `Vrm` root has existed (resets if the query fails).
    settle_frames: u32,
    /// True once the initial radius/force_update pass has run.
    initial_snap_done: bool,
    /// When set to `true` by [`recenter_on_orbit_zoom_input`], the next
    /// [`snap_orbit_focus_to_vrm_root`] call overrides the current focus to
    /// the VRM even if focus_follow_vrm is also driving frame-by-frame
    /// pinning. Cleared after use.
    force_recenter: bool,
}

fn spawn_orbit_camera(mut commands: Commands, settings: Res<Settings>) {
    let cam = &settings.camera;
    let gfx = &settings.graphics;
    let focus = Vec3::from_array(cam.focus);

    let mut orbit = PanOrbitCamera::default();
    orbit.focus = focus;
    orbit.target_focus = focus;
    orbit.target_radius = cam.initial_radius;
    orbit.radius = Some(cam.initial_radius);
    orbit.zoom_lower_limit = cam.min_radius;
    orbit.zoom_upper_limit = Some(cam.max_radius);
    orbit.orbit_sensitivity = cam.orbit_sensitivity;
    orbit.pan_sensitivity = cam.pan_sensitivity;
    orbit.zoom_sensitivity = cam.zoom_sensitivity;
    orbit.button_orbit = MouseButton::Left;
    orbit.button_pan = MouseButton::Middle;
    orbit.button_zoom = None;
    orbit.orbit_smoothness = cam.orbit_smoothness;
    orbit.zoom_smoothness = cam.zoom_smoothness;
    orbit.pan_smoothness = cam.pan_smoothness;

    let offset = Vec3::new(0.0, 0.25, cam.initial_radius);
    let eye = focus + offset;

    let msaa = msaa_from_settings(gfx.msaa_samples);

    let projection = Projection::Perspective(PerspectiveProjection {
        fov: cam.fov_y_radians,
        near: cam.near_clip.max(1e-4),
        far: cam.far_clip.max(cam.near_clip + 1.0),
        ..default()
    });

    let mut entity = commands.spawn((
        Transform::from_translation(eye).looking_at(focus, Vec3::Y),
        orbit,
        Camera3d::default(),
        projection,
        msaa,
        Exposure {
            ev100: gfx.exposure_ev100,
        },
    ));

    if gfx.hdr {
        entity.insert(Hdr);
    }
}

/// Keeps the camera's perspective near/far/FOV in sync with `Settings::camera`
/// so the Camera debug UI can tune them live.
fn apply_projection_settings(
    settings: Res<Settings>,
    mut cam_q: Query<&mut Projection, With<Camera3d>>,
) {
    if !settings.is_changed() {
        return;
    }
    let c = &settings.camera;
    for mut proj in &mut cam_q {
        if let Projection::Perspective(ref mut p) = *proj {
            let near = c.near_clip.max(1e-4);
            let far = c.far_clip.max(near + 1.0);
            let fov = c.fov_y_radians.clamp(0.1, std::f32::consts::PI - 0.1);
            if (p.near - near).abs() > f32::EPSILON
                || (p.far - far).abs() > f32::EPSILON
                || (p.fov - fov).abs() > f32::EPSILON
            {
                p.near = near;
                p.far = far;
                p.fov = fov;
            }
        }
    }
}

/// Flags `force_recenter` the moment the user begins a new orbit (LMB)
/// or zoom (scroll wheel) interaction, so [`snap_orbit_focus_to_vrm_root`]
/// will overwrite any pan-drifted focus on the next system run. Middle-click
/// pan is deliberately NOT a recenter trigger — panning is the one
/// interaction we want to persist.
fn recenter_on_orbit_zoom_input(
    settings: Res<Settings>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut scroll: MessageReader<MouseWheel>,
    mut state: ResMut<VrmFocusSnapState>,
) {
    if !settings.camera.recenter_on_orbit_zoom || !settings.camera.focus_follow_vrm {
        scroll.clear();
        return;
    }

    // Orbit press (start of a drag).
    if mouse.just_pressed(MouseButton::Left) {
        state.force_recenter = true;
    }

    // Any scroll tick — even smooth ones — is a zoom interaction.
    if scroll.read().next().is_some() {
        state.force_recenter = true;
    }
}

/// Keep the orbit focus pinned to the VRM root every frame when
/// `focus_follow_vrm` is true. To allow panning to drag the focus off the VRM,
/// the frame-by-frame pin is skipped while MMB (pan) is held down; the snap
/// is re-applied when the user starts a new orbit/zoom interaction (via
/// `force_recenter`) or on plain idle frames.
fn snap_orbit_focus_to_vrm_root(
    settings: Res<Settings>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut state: ResMut<VrmFocusSnapState>,
    vrm_tf: Query<&GlobalTransform, With<Vrm>>,
    mut orbit_q: Query<&mut PanOrbitCamera, With<Camera3d>>,
) {
    if !settings.camera.focus_follow_vrm {
        state.force_recenter = false;
        return;
    }

    let Ok(vrm_gtf) = vrm_tf.single() else {
        state.settle_frames = 0;
        return;
    };

    state.settle_frames = state.settle_frames.saturating_add(1);
    if state.settle_frames < settings.camera.snap_wait_frames {
        return;
    }

    let root = vrm_gtf.translation();
    if !root.is_finite() {
        return;
    }

    let lift = settings.camera.focus_y_lift;
    let target = root + Vec3::Y * lift;

    let panning = mouse.pressed(MouseButton::Middle);
    let force = std::mem::replace(&mut state.force_recenter, false);
    let initial = !state.initial_snap_done;

    // Don't fight the user while they are actively panning — that would make
    // pan feel like a rubber band. Still allow the initial post-load snap and
    // explicit force-recenters to win.
    if panning && !force && !initial {
        return;
    }

    for mut orbit in &mut orbit_q {
        orbit.focus = target;
        orbit.target_focus = target;

        if initial {
            orbit.force_update = true;
            let r = settings.camera.initial_radius;
            orbit.target_radius = r;
            orbit.radius = Some(r);
        }
    }

    if initial {
        state.initial_snap_done = true;
        info!(
            "orbit focus locked onto VRM root at ({:.2}, {:.2}, {:.2}) + Y lift {:.2}",
            root.x, root.y, root.z, lift
        );
    }
}
