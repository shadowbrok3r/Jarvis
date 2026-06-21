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
use bevy::camera::Hdr;
use bevy::window::{PrimaryWindow, Window};
use bevy_egui::{EguiContexts, egui};
use bevy_panorbit_camera::{PanOrbitCamera, PanOrbitCameraPlugin, PanOrbitCameraSystemSet};
use bevy_vrm1::prelude::Vrm;

use jarvis_avatar::config::{Settings, msaa_from_settings};

use crate::plugins::pose_driver::IndexedBones;
use crate::plugins::rig_editor::RigEditorState;

/// Maximum perpendicular distance (m) from the click ray to a bone joint
/// for the joint to count as "the user clicked on the mesh near here". The
/// bone skeleton is a coarse proxy for the rendered mesh — 0.35 m is wide
/// enough to catch torso / face hits without snapping to a far-away limb
/// when the click landed in empty space.
const CLICK_PIVOT_BONE_RADIUS_M: f32 = 0.35;
/// Min orbit radius as a fraction of skeleton extent (root → farthest bone).
const ZOOM_FLOOR_EXTENT_FRAC: f32 = 0.15;
/// Min camera distance to any indexed bone (m at `uniform_scale` 1.0).
const SHELL_BASE_M: f32 = 0.09;
/// Perspective near plane as a fraction of orbit radius (scales in when zoomed in).
const NEAR_CLIP_RADIUS_FRAC: f32 = 0.02;
/// Near plane when bone-shell distance is known (tighter than radius-based guess).
const NEAR_CLIP_NEAREST_FRAC: f32 = 0.12;
const NEAR_CLIP_ABSOLUTE_MIN: f32 = 5e-5;

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
                    set_orbit_pivot_from_click,
                    snap_orbit_focus_to_vrm_root,
                    apply_projection_settings,
                )
                    .chain(),
            )
            .add_systems(
                PostUpdate,
                sync_dynamic_camera_clip.after(PanOrbitCameraSystemSet),
            )
            // List-click in the Bones tab (in edit mode) is the only producer
            // of `pending_focus_camera_to_bone` — viewport mesh picks
            // deliberately don't snap the camera per user request. The
            // explicit `.after` keeps the viewport-pick system as a stable
            // ordering anchor inside `Update` even though the pick itself no
            // longer writes the focus request.
            .add_systems(
                Update,
                focus_camera_on_selected_bone
                    .after(crate::plugins::debug_ui::rig_editor::rig_editor_viewport_pick),
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
        Ok(ctx) => ctx.egui_wants_pointer_input(),
        Err(_) => false,
    };

    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    // Two distinct paths suppress orbit:
    //  1. Plain LMB drag that started over an axis ring (`dragging_axis`).
    //  2. Alt + LMB anywhere over the model with a bone selected (legacy
    //     freeform twist) — kept so users with the previous muscle memory
    //     don't lose the gesture after the move into the Pose Controller.
    let lmb_held = mouse.pressed(MouseButton::Left);
    let axis_ring_drag =
        rig.edit_mode && rig.dragging_axis.is_some() && lmb_held && !egui_blocks;
    let alt_twist = rig.edit_mode
        && rig.twist_drag_enabled
        && rig.selected_bone.is_some()
        && alt
        && lmb_held
        && !egui_blocks;
    let suppress = axis_ring_drag || alt_twist;

    for mut orbit in &mut orbit_q {
        if suppress {
            if gate.saved_enabled.is_none() {
                gate.saved_enabled = Some(orbit.enabled);
            }
            orbit.enabled = false;
        } else if let Some(prev) = gate.saved_enabled.take() {
            orbit.enabled = prev;
        }
    }
}

/// Consumes [`RigEditorState::pending_focus_camera_to_bone`] one-shot requests
/// — when the user clicks a bone in the list / the rig tab's "Focus camera"
/// button, we move the orbit pivot so the bone appears at the center of the
/// **visible** viewport (the egui central-panel area, after side and bottom
/// panels are subtracted). Without this offset the bone snaps to the
/// physical window center, which is hidden behind whatever side panel is
/// docked there.
///
/// We compute the offset by:
///   1. Asking egui for `ctx.available_rect()` — that's the rectangle left
///      over for the 3D viewport.
///   2. Computing how far that rect's center is from the window center, in
///      logical pixels, and converting to physical pixels with the egui
///      pixels-per-point scaling.
///   3. Translating that screen-space offset into world units at the bone's
///      depth using the camera's vertical FOV, then nudging the focus along
///      the camera's right/up basis vectors.
///
/// We don't pin it (the existing `snap_orbit_focus_to_vrm_root` logic
/// eventually re-asserts the rig root if `focus_follow_vrm` is on), so this
/// is genuinely one-shot.
fn focus_camera_on_selected_bone(
    mut rig: ResMut<RigEditorState>,
    mut contexts: EguiContexts,
    windows: Query<&Window, With<PrimaryWindow>>,
    indexed: Option<Res<IndexedBones>>,
    gtf_q: Query<&GlobalTransform>,
    mut orbit_q: Query<(&mut PanOrbitCamera, &GlobalTransform, &Projection), With<Camera3d>>,
    mut snap_state: ResMut<VrmFocusSnapState>,
) {
    let Some(bone) = rig.pending_focus_camera_to_bone.take() else {
        return;
    };
    let Some(indexed) = indexed else {
        return;
    };
    let Some(entity) = indexed.entity(&bone) else {
        return;
    };
    let Ok(gtf) = gtf_q.get(entity) else {
        return;
    };
    let p = gtf.translation();
    if !p.is_finite() {
        return;
    }

    let available = visible_viewport_offset(&mut contexts, windows.single().ok());

    for (mut orbit, cam_gt, projection) in &mut orbit_q {
        let cam_pos = cam_gt.translation();
        let forward = cam_gt.forward().as_vec3();
        let right = cam_gt.right().as_vec3();
        let up = cam_gt.up().as_vec3();

        // Distance from the camera plane to the bone — used to convert
        // screen-space pixels into world units at the bone's depth.
        let depth = forward.dot(p - cam_pos).max(0.05);

        // Pixel -> world scale at this depth. Vertical FOV is the canonical
        // axis; horizontal is derived via aspect ratio = window_w / window_h.
        let world_per_pixel = if let Some((fov_y, win_w, win_h)) =
            available.viewport_metrics(projection)
        {
            // 2 * d * tan(fov/2) / window_height_pixels gives world units per
            // logical pixel of vertical movement; horizontal shares the same
            // factor since both are measured against window_height.
            let _ = (win_w, win_h);
            (2.0 * depth * (fov_y * 0.5).tan()) / win_h
        } else {
            0.0
        };

        // Visible-area center is offset from window center by these many
        // pixels. The focus should end up offset from the bone in the
        // OPPOSITE direction so the bone shows up at visible center.
        let dx_px = available.dx_to_visible_center_pixels;
        let dy_px = available.dy_to_visible_center_pixels;
        let dx_world = dx_px * world_per_pixel;
        let dy_world = dy_px * world_per_pixel;

        // Screen Y points down, world up points up — flip Y.
        let focus = p - right * dx_world + up * dy_world;

        orbit.focus = focus;
        orbit.target_focus = focus;
        orbit.force_update = true;
    }

    // Pause the VRM-root re-pin for a few frames so the user can look at the
    // bone before focus snaps back. The user can also turn off focus_follow_vrm
    // in Settings → Camera if they want sticky bone-focus.
    snap_state.force_recenter = false;
    snap_state.settle_frames = 0;
}

/// Cached rectangle metrics so the math in `focus_camera_on_selected_bone`
/// stays readable. `dx_to_visible_center_pixels` / `dy_to_visible_center_pixels`
/// are in **physical** pixels — egui returns logical points, multiplied by
/// `pixels_per_point` to match the camera viewport's pixel coordinate
/// system. `physical_size` tracks the full window in physical pixels so the
/// pixel-to-world conversion can divide by `window_height_px`.
struct VisibleViewportInfo {
    dx_to_visible_center_pixels: f32,
    dy_to_visible_center_pixels: f32,
    physical_size: Option<(f32, f32)>,
}

impl VisibleViewportInfo {
    fn viewport_metrics(&self, projection: &Projection) -> Option<(f32, f32, f32)> {
        let (w, h) = self.physical_size?;
        let fov_y = match projection {
            Projection::Perspective(p) => p.fov,
            _ => return None,
        };
        Some((fov_y, w, h))
    }
}

fn visible_viewport_offset(
    contexts: &mut EguiContexts,
    window: Option<&Window>,
) -> VisibleViewportInfo {
    let Ok(ctx) = contexts.ctx_mut() else {
        return VisibleViewportInfo {
            dx_to_visible_center_pixels: 0.0,
            dy_to_visible_center_pixels: 0.0,
            physical_size: None,
        };
    };

    let pixels_per_point = ctx.pixels_per_point();
    // `viewport_rect()` is the full window area in egui points (the
    // replacement for the deprecated `screen_rect()`), and
    // `available_rect()` is what's left after side / bottom panels.
    let screen_rect: egui::Rect = ctx.viewport_rect();
    let available_rect: egui::Rect = ctx.content_rect();

    let screen_center = screen_rect.center();
    let visible_center = available_rect.center();
    let dx_pts = visible_center.x - screen_center.x;
    let dy_pts = visible_center.y - screen_center.y;

    let physical_size = window.map(|w| {
        (
            w.physical_width() as f32,
            w.physical_height() as f32,
        )
    });

    VisibleViewportInfo {
        dx_to_visible_center_pixels: dx_pts * pixels_per_point,
        dy_to_visible_center_pixels: dy_pts * pixels_per_point,
        physical_size,
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

/// Keeps perspective far/FOV in sync with `Settings::camera`. Near clip is
/// driven every frame by [`sync_dynamic_camera_clip`].
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
            let far = c.far_clip.max(c.near_clip + 1.0);
            let fov = c.fov_y_radians.clamp(0.1, std::f32::consts::PI - 0.1);
            if (p.far - far).abs() > f32::EPSILON || (p.fov - fov).abs() > f32::EPSILON {
                p.far = far;
                p.fov = fov;
            }
        }
    }
}

fn character_extent_from_bones(
    root: Vec3,
    indexed: Option<&IndexedBones>,
    gtf_q: &Query<&GlobalTransform>,
) -> Option<f32> {
    let indexed = indexed?;
    if indexed.entities.is_empty() {
        return None;
    }
    let mut max_d = 0.25f32;
    for (_, entity) in &indexed.entities {
        let Ok(gtf) = gtf_q.get(*entity) else {
            continue;
        };
        let p = gtf.translation();
        if p.is_finite() {
            max_d = max_d.max((p - root).length());
        }
    }
    Some(max_d)
}

fn nearest_bone_distance(
    cam_pos: Vec3,
    indexed: Option<&IndexedBones>,
    gtf_q: &Query<&GlobalTransform>,
) -> Option<f32> {
    let indexed = indexed?;
    let mut nearest = f32::MAX;
    for (_, entity) in &indexed.entities {
        let Ok(gtf) = gtf_q.get(*entity) else {
            continue;
        };
        let p = gtf.translation();
        if p.is_finite() {
            nearest = nearest.min(cam_pos.distance(p));
        }
    }
    (nearest.is_finite() && nearest < f32::MAX).then_some(nearest)
}

/// Scale near clip with zoom, keep orbit outside the mesh shell, honor avatar scale.
fn sync_dynamic_camera_clip(
    settings: Res<Settings>,
    indexed: Option<Res<IndexedBones>>,
    vrm_q: Query<(&GlobalTransform, &Transform), With<Vrm>>,
    gtf_q: Query<&GlobalTransform>,
    cam_gt_q: Query<&GlobalTransform, With<Camera3d>>,
    mut orbit_q: Query<&mut PanOrbitCamera, With<Camera3d>>,
    mut proj_q: Query<&mut Projection, With<Camera3d>>,
) {
    let cam = &settings.camera;
    let Ok((vrm_gtf, vrm_tf)) = vrm_q.single() else {
        return;
    };
    let uniform_scale = vrm_tf.scale.x.max(0.001);
    let shell = SHELL_BASE_M * uniform_scale;

    let extent = character_extent_from_bones(vrm_gtf.translation(), indexed.as_deref(), &gtf_q);
    let zoom_floor = extent
        .map(|e| cam.min_radius.max(e * ZOOM_FLOOR_EXTENT_FRAC).max(shell * 0.5))
        .unwrap_or(cam.min_radius.max(shell * 0.5));

    let cam_pos = cam_gt_q.single().ok().map(|gtf| gtf.translation());
    let nearest_bone = cam_pos.and_then(|p| nearest_bone_distance(p, indexed.as_deref(), &gtf_q));

    for mut orbit in &mut orbit_q {
        orbit.zoom_lower_limit = zoom_floor;
        orbit.zoom_upper_limit = Some(cam.max_radius.max(zoom_floor + 0.01));

        let mut radius = orbit
            .radius
            .unwrap_or(orbit.target_radius)
            .max(zoom_floor);

        if let Some(nearest) = nearest_bone {
            if nearest < shell {
                let push = shell / nearest.max(1e-4);
                radius = (radius * push).max(zoom_floor);
                orbit.target_radius = radius;
                orbit.radius = Some(radius);
                orbit.force_update = true;
            }
        } else if orbit.target_radius < zoom_floor {
            orbit.target_radius = zoom_floor;
            orbit.radius = Some(zoom_floor);
            radius = zoom_floor;
        } else if orbit.radius.map(|r| r < zoom_floor).unwrap_or(false) {
            orbit.radius = Some(zoom_floor);
            radius = zoom_floor;
        }

        let dynamic_near = nearest_bone
            .map(|d| (d * NEAR_CLIP_NEAREST_FRAC).min(cam.near_clip))
            .unwrap_or_else(|| (radius * NEAR_CLIP_RADIUS_FRAC).min(cam.near_clip))
            .max(NEAR_CLIP_ABSOLUTE_MIN);

        for mut proj in &mut proj_q {
            if let Projection::Perspective(ref mut p) = *proj {
                let far = cam.far_clip.max(dynamic_near + 1.0);
                if (p.near - dynamic_near).abs() > f32::EPSILON {
                    p.near = dynamic_near;
                }
                if (p.far - far).abs() > f32::EPSILON {
                    p.far = far;
                }
            }
        }
    }
}

/// On LMB just-pressed, set the orbit pivot to whatever's under the cursor
/// **without moving the camera**. The pivot change becomes visible only
/// when the user actually starts orbiting (LMB drag) — pure click + release
/// is invisible.
///
/// * **Cursor over the mesh** → pivot is the point on the click ray at the
///   depth of the nearest bone joint within
///   [`CLICK_PIVOT_BONE_RADIUS_M`]. Subsequent orbit revolves around the
///   exact spot the user clicked on.
/// * **Cursor in empty space** → pivot is the VRM root (so orbiting in
///   empty space still frames the model).
///
/// To avoid the "camera shoots over" failure mode, after computing the new
/// focus we recompute `radius` / `yaw` / `pitch` from the camera's current
/// world position relative to the new focus, and write `focus`,
/// `target_focus`, `radius`, `target_radius`, `yaw`, `target_yaw`, `pitch`,
/// `target_pitch` together. This keeps the camera physically still while
/// changing what it orbits around — the same trick Blender's "set camera
/// pivot to cursor" gesture uses.
///
/// Skipped while egui owns pointer input (so clicking the F1 panel never
/// jacks the camera) or while the rig editor is grabbing an axis ring.
#[allow(clippy::too_many_arguments)]
fn set_orbit_pivot_from_click(
    settings: Res<Settings>,
    mut contexts: EguiContexts,
    mouse: Res<ButtonInput<MouseButton>>,
    rig: Res<RigEditorState>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cam_q: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    indexed: Option<Res<IndexedBones>>,
    gtf_q: Query<&GlobalTransform>,
    vrm_q: Query<&GlobalTransform, With<Vrm>>,
    mut orbit_q: Query<&mut PanOrbitCamera, With<Camera3d>>,
    mut state: ResMut<VrmFocusSnapState>,
) {
    if !settings.camera.click_pivot_orbit {
        return;
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    if matches!(contexts.ctx_mut(), Ok(ctx) if ctx.egui_wants_pointer_input()) {
        return;
    }
    if rig.dragging_axis.is_some() || rig.hovered_axis.is_some() {
        return;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((cam, cam_gt)) = cam_q.single() else {
        return;
    };
    let Ok(ray) = cam.viewport_to_world(cam_gt, cursor) else {
        return;
    };

    // Find the bone joint closest to the click ray within the threshold.
    // Use the smallest forward-distance `t` so foreground hits beat
    // background ones (e.g. nose beats head's back of skull).
    let mut best: Option<(f32, Vec3)> = None;
    if let Some(indexed) = indexed.as_deref() {
        for (_name, entity) in &indexed.entities {
            let Ok(gtf) = gtf_q.get(*entity) else {
                continue;
            };
            let p = gtf.translation();
            if !p.is_finite() {
                continue;
            }
            let to = p - ray.origin;
            let t = to.dot(*ray.direction);
            if t <= 0.0 {
                continue;
            }
            let closest = ray.origin + *ray.direction * t;
            let d = (p - closest).length();
            if d > CLICK_PIVOT_BONE_RADIUS_M {
                continue;
            }
            let replace = best.as_ref().map(|(bt, _)| t < *bt).unwrap_or(true);
            if replace {
                best = Some((t, closest));
            }
        }
    }

    let pivot = match best {
        Some((_, p)) => p,
        None => {
            let Ok(vrm_gtf) = vrm_q.single() else {
                return;
            };
            vrm_gtf.translation() + Vec3::Y * settings.camera.focus_y_lift
        }
    };

    if !pivot.is_finite() {
        return;
    }

    let cam_pos = cam_gt.translation();
    let offset = cam_pos - pivot;
    let new_radius = offset.length();
    if new_radius < 1.0e-4 {
        return; // pivot is on top of the camera — pathological, leave it alone
    }
    let dir = offset / new_radius;
    // PanOrbitCamera spherical convention: at yaw=pitch=0 the offset from
    // focus to camera is `+Z`. Yaw rotates around global Y, pitch around
    // local X (post-yaw). So:
    //   pitch = asin(dir.y)
    //   yaw   = atan2(dir.x, dir.z)
    let new_pitch = dir.y.clamp(-1.0, 1.0).asin();
    let new_yaw = dir.x.atan2(dir.z);

    for mut orbit in &mut orbit_q {
        orbit.focus = pivot;
        orbit.target_focus = pivot;
        orbit.radius = Some(new_radius);
        orbit.target_radius = new_radius;
        orbit.yaw = Some(new_yaw);
        orbit.target_yaw = new_yaw;
        orbit.pitch = Some(new_pitch);
        orbit.target_pitch = new_pitch;
        // No `force_update` — every `target_*` matches the resolved value,
        // so the camera transform comes out identical to the current one.
    }

    // Defuse the recenter-on-LMB flag set by `recenter_on_orbit_zoom_input`
    // and stall the VRM-root re-pin so the user gets to actually orbit
    // around the chosen pivot for `snap_wait_frames` ticks.
    state.force_recenter = false;
    state.settle_frames = 0;
}

/// Flags `force_recenter` the moment the user begins a new orbit (LMB)
/// or zoom (scroll wheel) interaction, so [`snap_orbit_focus_to_vrm_root`]
/// will overwrite any pan-drifted focus on the next system run. Middle-click
/// pan is deliberately NOT a recenter trigger — panning is the one
/// interaction we want to persist.
///
/// `set_orbit_pivot_from_click` runs immediately after this and may clear
/// `force_recenter` again — that's intentional: zoom (scroll) keeps the
/// VRM-root behavior, but LMB orbit hands off to the click-pivot system.
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
    // When the experimental click-pivot orbit is on, we also need to honour
    // an LMB-orbit gesture (otherwise `focus_follow_vrm` snaps the focus
    // back to the VRM root mid-orbit and the camera jumps). When the flag
    // is off, the previous "MMB-only" guard is correct.
    let lmb_orbit_in_progress =
        settings.camera.click_pivot_orbit && mouse.pressed(MouseButton::Left);
    let force = std::mem::replace(&mut state.force_recenter, false);
    let initial = !state.initial_snap_done;

    // Don't fight the user while they're actively interacting — panning
    // (MMB) and (when click-pivot is enabled) orbiting (LMB) are both
    // gestures we want to honour. Initial post-load snap and explicit
    // force-recenters still win.
    if (panning || lmb_orbit_in_progress) && !force && !initial {
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
