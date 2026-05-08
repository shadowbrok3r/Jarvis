//! [`RigEditorState`] (shared UI + pick settings) and in-world axis gizmo lines
//! for the selected bone.
//!
//! Viewport **pick / hover / axis-drag** systems live in
//! [`crate::plugins::debug_ui::rig_editor`] so they can update
//! [`crate::plugins::debug_ui::DebugUiState`] without a `plugins` dependency cycle.
//!
//! Rig gizmos use a dedicated [`RigEditorGizmoGroup`] with strongly negative
//! [`GizmoConfig::depth_bias`] so axes stay visible inside dense meshes.

use bevy::prelude::*;

use crate::plugins::pose_driver::IndexedBones;

/// Gizmo layer for rig editor axes / joint marker — drawn in front of opaque geometry.
#[derive(Default, Reflect, GizmoConfigGroup)]
#[reflect(Default)]
pub struct RigEditorGizmoGroup;

fn rig_editor_gizmo_config() -> GizmoConfig {
    let mut c = GizmoConfig::default();
    // -1 = as close to the camera as depth bias allows (see `bevy_gizmos` docs).
    c.depth_bias = -1.0;
    c.line.width = 3.0;
    c
}

pub struct RigEditorPlugin;

impl Plugin for RigEditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RigEditorState>()
            .init_resource::<crate::plugins::mirror::MirrorState>()
            .insert_gizmo_config(RigEditorGizmoGroup::default(), rig_editor_gizmo_config())
            .add_systems(
                PostUpdate,
                rig_editor_draw_gizmo.after(TransformSystems::Propagate),
            );
    }
}

/// Which local-space axis of the selected bone is currently the active rotation
/// target for sliders / mouse drags. Gizmo rendering colors X red / Y green /
/// Z blue (Blender convention).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum RigEditAxis {
    X,
    Y,
    Z,
}

impl RigEditAxis {
    pub fn as_idx(self) -> usize {
        match self {
            RigEditAxis::X => 0,
            RigEditAxis::Y => 1,
            RigEditAxis::Z => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RigEditAxis::X => "X",
            RigEditAxis::Y => "Y",
            RigEditAxis::Z => "Z",
        }
    }

    /// Bone-local unit axis vector.
    pub fn unit(self) -> Vec3 {
        match self {
            RigEditAxis::X => Vec3::X,
            RigEditAxis::Y => Vec3::Y,
            RigEditAxis::Z => Vec3::Z,
        }
    }

    /// RGB color used for in-world rings, axis lines, and UI tab accents.
    pub fn base_color(self) -> Color {
        match self {
            RigEditAxis::X => Color::srgb(0.92, 0.30, 0.30),
            RigEditAxis::Y => Color::srgb(0.35, 0.90, 0.40),
            RigEditAxis::Z => Color::srgb(0.40, 0.55, 0.95),
        }
    }

    /// Brighter color used when this axis is hovered.
    pub fn hot_color(self) -> Color {
        match self {
            RigEditAxis::X => Color::srgb(1.0, 0.65, 0.65),
            RigEditAxis::Y => Color::srgb(0.70, 1.0, 0.75),
            RigEditAxis::Z => Color::srgb(0.65, 0.85, 1.0),
        }
    }

    /// Maximally vivid / saturated color reserved for the *active* drag handle
    /// (the one being dragged or last-selected). Rings and lines never use
    /// this — only the handle blob — so the user can pinpoint "which axis am
    /// I currently turning" at a glance.
    pub fn vivid_color(self) -> Color {
        match self {
            RigEditAxis::X => Color::srgb(1.0, 0.10, 0.10),
            RigEditAxis::Y => Color::srgb(0.10, 1.0, 0.20),
            RigEditAxis::Z => Color::srgb(0.20, 0.45, 1.0),
        }
    }
}

/// Multiply the alpha component of `c` by `factor` (clamped to 0..=1).
pub fn fade_color(c: Color, factor: f32) -> Color {
    let s = c.to_srgba();
    Color::srgba(s.red, s.green, s.blue, (s.alpha * factor).clamp(0.0, 1.0))
}

#[derive(Resource)]
pub struct RigEditorState {
    /// Master toggle: when on, the viewport runs hover / pick / axis-drag
    /// systems against the loaded VRM. When off, the rig editor's mouse
    /// behaviors are disabled and the gizmo is drawn translucently for
    /// reference only.
    pub edit_mode: bool,
    /// While true, alt+LMB drag adds rotation to the selected bone around
    /// [`Self::active_axis`] (was previously hardcoded to local Z).
    pub twist_drag_enabled: bool,
    /// Bone the user has explicitly selected (via mesh click or list click).
    pub selected_bone: Option<String>,
    /// Bone currently under the cursor (mesh hover) or under the egui list
    /// row's pointer; whichever set it most recently. One-shot per frame.
    pub hovered_bone: Option<String>,
    /// Source that wrote `hovered_bone` last frame — used so the viewport
    /// hover system can yield to the egui list (and vice versa) when the
    /// user moves the cursor to the other surface.
    pub hovered_source: HoverSource,
    /// Axis circle currently under the cursor in the viewport, if any.
    pub hovered_axis: Option<RigEditAxis>,
    /// Axis the user has clicked on / picked manually for slider + drag.
    pub active_axis: RigEditAxis,
    /// Set on `mouse.just_pressed(LMB)` over an axis circle; cleared on
    /// `mouse.just_released(LMB)`. Persists across frames so the drag does
    /// not "fall off" if the cursor moves away from the ring during the
    /// gesture.
    pub dragging_axis: Option<RigEditAxis>,
    /// One-shot: when set, the bones list scrolls the matching row into view
    /// next frame, then clears.
    pub pending_scroll_to_bone: Option<String>,
    /// One-shot: when set, the orbit camera moves its focus onto the bone's
    /// world position next frame, then clears. List click in edit mode is
    /// the only producer — the viewport mesh pick deliberately does NOT
    /// snap the camera (per user request: viewport selection is "I want to
    /// pose this bone", not "look at it").
    pub pending_focus_camera_to_bone: Option<String>,
    /// Mesh-pick tube radius (meters) for bone hover / click.
    pub pick_radius_m: f32,
    /// Pixels of cursor proximity required to register an axis-handle hover.
    /// Only the small filled handle on each ring is interactive now — the ring
    /// outline itself is purely visual feedback. A larger value here makes
    /// handles easier to grab without enlarging the visual radius.
    pub axis_pick_radius_px: f32,
    /// Visual radius (meters) of the colored rotation rings around the
    /// selected joint.
    pub gizmo_radius_m: f32,
    /// Degrees of rotation per pixel of horizontal mouse drag.
    pub twist_drag_sensitivity: f32,
    /// When `true`, dragging the cursor to the right rotates *negative* on
    /// the active axis (so the bone visually appears to follow the cursor
    /// in the inverted convention). Defaults to `false` (right = positive).
    pub invert_drag_direction: bool,
    /// Bone-pick highlight circle: radius (m), opacity, hover color, select
    /// color. The hover/select markers are drawn as camera-facing circles so
    /// they read as "rings around the joint" — easier to spot in cluttered
    /// finger / face areas than a tiny sphere was.
    pub bone_pick_marker_radius_m: f32,
    pub bone_pick_marker_alpha: f32,
    pub bone_pick_hover_color: [f32; 3],
    pub bone_pick_select_color: [f32; 3],
    /// Base opacity of the ring outlines + axis lines (the *visual* part).
    /// When the user hovers a handle the **other** axes' rings/handles get
    /// multiplied by [`Self::ring_dim_factor`] so the focused axis pops.
    pub ring_alpha: f32,
    /// Base opacity of the axis-handle dots (the *interactive* part) — kept
    /// higher than `ring_alpha` so handles always read as the click target.
    pub handle_alpha: f32,
    /// Multiplier applied to non-hovered axes when one axis is hot. 1.0 = no
    /// dimming, 0.0 = invisible.
    pub ring_dim_factor: f32,
    /// Sensitivity multiplier when the user holds Shift. Default 0.15 ≈
    /// "0.15° per pixel" for micro-movements; matches Blender's modal-style
    /// shift-precision for transform gizmos.
    pub shift_precision_factor: f32,
    pub last_pick_message: Option<String>,
    /// Rig editor → VRMC spring joints / colliders panel (filter + grouping + preset UX).
    pub spring_ui: RigEditorSpringUiState,
}

/// Where the most recent `hovered_bone` write came from. Lets the viewport
/// system avoid clobbering a fresh list-hover (and vice versa) within the
/// same frame.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum HoverSource {
    #[default]
    None,
    Viewport,
    List,
}

/// UI-only state for spring joint / collider lists in the Rig editor window.
#[derive(Clone, Debug)]
pub struct RigEditorSpringUiState {
    pub joint_filter: String,
    /// 0 = all, 1 = name prefix, 2 = VRMC spring chain name
    pub joint_group_mode: u8,
    pub joint_group_value: String,
    pub collider_filter: String,
    /// 0 = all, 1 = shape kind, 2 = VRMC spring chain (collider host → chain)
    pub collider_group_mode: u8,
    pub collider_group_value: String,
    pub preset_status: Option<String>,
}

impl Default for RigEditorSpringUiState {
    fn default() -> Self {
        Self {
            joint_filter: String::new(),
            joint_group_mode: 0,
            joint_group_value: String::new(),
            collider_filter: String::new(),
            collider_group_mode: 0,
            collider_group_value: String::new(),
            preset_status: None,
        }
    }
}

impl Default for RigEditorState {
    fn default() -> Self {
        Self {
            edit_mode: false,
            twist_drag_enabled: true,
            selected_bone: None,
            hovered_bone: None,
            hovered_source: HoverSource::None,
            hovered_axis: None,
            active_axis: RigEditAxis::Y,
            dragging_axis: None,
            pending_scroll_to_bone: None,
            pending_focus_camera_to_bone: None,
            pick_radius_m: 0.08,
            axis_pick_radius_px: 22.0,
            gizmo_radius_m: 0.08,
            twist_drag_sensitivity: 0.35,
            invert_drag_direction: false,
            bone_pick_marker_radius_m: 0.020,
            bone_pick_marker_alpha: 0.85,
            bone_pick_hover_color: [1.0, 0.85, 0.20],
            bone_pick_select_color: [0.55, 0.85, 1.0],
            ring_alpha: 0.55,
            handle_alpha: 1.0,
            ring_dim_factor: 0.20,
            shift_precision_factor: 0.15,
            last_pick_message: None,
            spring_ui: RigEditorSpringUiState::default(),
        }
    }
}

/// Returns the world-space rotation that places a `gizmos.circle` so that its
/// plane is perpendicular to the bone-frame `axis` (i.e. the ring lies in the
/// rotation plane traced when twisting that axis).
///
/// `gizmos.circle(Isometry3d::new(p, q), r, c)` draws a circle in the rotation
/// frame's local XY plane (normal = local Z). To make the ring perpendicular
/// to axis `a` (in bone-frame), we want local Z → frame_rot * a.
pub fn axis_ring_rotation(frame_rot: Quat, axis: RigEditAxis) -> Quat {
    let prealign = match axis {
        // Map local Z → +X (rotate +90° about local Y).
        RigEditAxis::X => Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        // Map local Z → +Y (rotate -90° about local X).
        RigEditAxis::Y => Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        // Local Z is already +Z.
        RigEditAxis::Z => Quat::IDENTITY,
    };
    frame_rot * prealign
}

/// World-space rotation that defines the rendering / drag frame for the
/// axis gizmo. Returns the bone's **current** world rotation, so the rings
/// stay glued to the bone as it rotates (per the user's "axis stays
/// aligned to the bone" request). Combined with local-frame composition
/// in [`crate::plugins::debug_ui::rig_editor::rig_editor_axis_drag`]
/// (`current_q * delta_q`), dragging an axis ring rotates the bone around
/// its own current local axis — so the ring visibly spins around itself.
///
/// We deliberately ignore the parent's bind/rest decomposition here: that
/// math was only needed when we composed in the bind-local frame
/// (`delta_q * current_q`). With the local-frame post-composition the
/// drag axis is the bone's current local axis, which is naturally
/// expressed by `bone_world * X`, `bone_world * Y`, `bone_world * Z`.
///
/// `gtf_q` is the live world transforms. `rest_gtf_q` and `child_of_q`
/// are kept in the signature for callsite stability — they're not read
/// today but allow re-introducing parent-aware fallbacks without churning
/// every caller again.
pub fn bone_bind_world_rot(
    entity: Entity,
    gtf_q: &Query<&GlobalTransform>,
    _rest_gtf_q: &Query<&bevy_vrm1::prelude::RestGlobalTransform>,
    _child_of_q: &Query<&ChildOf>,
) -> Quat {
    gtf_q
        .get(entity)
        .map(|g| g.rotation())
        .unwrap_or(Quat::IDENTITY)
}

/// Per-axis "rest" angle (radians) for the drag handle on its ring. Spread
/// across the rings so the green and blue handles don't overlap when the
/// bone is at bind:
///
/// * X handle at angle 0  → ring's local +X (world `-Z` for identity bind)
/// * Y handle at angle 0  → ring's local +X (world `+X` for identity bind)
/// * Z handle at angle π/2 → ring's local +Y (world `+Y` for identity bind)
///
/// All three sit on different world directions for an identity bind, and
/// stay distinct under any rigid bind transform.
pub fn axis_handle_base_angle(axis: RigEditAxis) -> f32 {
    match axis {
        RigEditAxis::X => 0.0,
        RigEditAxis::Y => 0.0,
        RigEditAxis::Z => std::f32::consts::FRAC_PI_2,
    }
}

/// Effective handle angle on its ring = base + accumulated drag (in radians).
/// We use the bone's current Euler component as the visualization of "how
/// far you've rotated this axis". For multi-axis poses the Euler decomposes
/// from a quaternion, so this is approximate — but it correctly tracks
/// single-axis drags (which is the dominant rig-editor gesture) and remains
/// continuous through the others.
pub fn axis_handle_angle(axis: RigEditAxis, euler_deg: [f32; 3]) -> f32 {
    axis_handle_base_angle(axis) + euler_deg[axis.as_idx()].to_radians()
}

/// World-space position of the drag handle for `axis` on the ring centered
/// at `centre` in `frame_rot` with the given `radius` and current
/// `angle_rad` (computed via [`axis_handle_angle`]).
pub fn axis_handle_world(
    centre: Vec3,
    frame_rot: Quat,
    axis: RigEditAxis,
    radius: f32,
    angle_rad: f32,
) -> Vec3 {
    let q = axis_ring_rotation(frame_rot, axis);
    let local = Vec3::new(angle_rad.cos() * radius, angle_rad.sin() * radius, 0.0);
    centre + q * local
}

fn rig_editor_draw_gizmo(
    mut gizmos: Gizmos<RigEditorGizmoGroup>,
    rig: Res<RigEditorState>,
    indexed: Option<Res<IndexedBones>>,
    gtf_q: Query<&GlobalTransform>,
    rest_gtf_q: Query<&bevy_vrm1::prelude::RestGlobalTransform>,
    child_of_q: Query<&ChildOf>,
    cam_q: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    debug: Res<crate::plugins::debug_ui::DebugUiState>,
) {
    let Some(indexed) = indexed else {
        return;
    };

    // Camera direction so the bone-pick markers can be drawn as
    // billboard-style circles ("circle around the joint", per user request)
    // instead of tiny depth-occluded spheres.
    let cam_pos = cam_q
        .iter()
        .next()
        .map(|(_, gt)| gt.translation())
        .unwrap_or(Vec3::ZERO);
    let billboard_rot = |joint: Vec3| -> Quat {
        let to_cam = (cam_pos - joint).normalize_or_zero();
        if to_cam.length_squared() < 1.0e-6 {
            Quat::IDENTITY
        } else {
            Quat::from_rotation_arc(Vec3::Z, to_cam)
        }
    };
    let alpha = rig.bone_pick_marker_alpha.clamp(0.0, 1.0);
    let marker_radius = rig.bone_pick_marker_radius_m.max(0.002);
    let hover_color = Color::srgba(
        rig.bone_pick_hover_color[0],
        rig.bone_pick_hover_color[1],
        rig.bone_pick_hover_color[2],
        alpha,
    );
    let select_color = Color::srgba(
        rig.bone_pick_select_color[0],
        rig.bone_pick_select_color[1],
        rig.bone_pick_select_color[2],
        alpha,
    );

    // Hover preview (Blender-like) — bone segment + camera-facing circle.
    if let Some(name) = rig.hovered_bone.as_deref() {
        if rig.selected_bone.as_deref() != Some(name) {
            if let Some(entity) = indexed.entity(name) {
                if let Ok(gtf) = gtf_q.get(entity) {
                    let p = gtf.translation();
                    if let Ok(co) = child_of_q.get(entity) {
                        if let Ok(parent_gtf) = gtf_q.get(co.parent()) {
                            let p0 = parent_gtf.translation();
                            gizmos.line(p0, p, hover_color);
                        }
                    }
                    gizmos.circle(
                        Isometry3d::new(p, billboard_rot(p)),
                        marker_radius,
                        hover_color,
                    );
                }
            }
        }
    }

    let Some(bone) = rig.selected_bone.as_deref() else {
        return;
    };
    let Some(entity) = indexed.entity(bone) else {
        return;
    };
    let Ok(gtf) = gtf_q.get(entity) else {
        return;
    };
    let p = gtf.translation();
    // Effective rotation frame for this bone — see `bone_bind_world_rot`.
    let r = bone_bind_world_rot(entity, &gtf_q, &rest_gtf_q, &child_of_q);

    // Selected bone segment (parent → joint), brighter than the hover stroke
    // so the user can tell selected vs hovered apart.
    if let Ok(co) = child_of_q.get(entity) {
        if let Ok(parent_gtf) = gtf_q.get(co.parent()) {
            let p0 = parent_gtf.translation();
            gizmos.line(p0, p, select_color);
        }
    }

    // Three colored rotation rings + a filled drag handle on each.
    //
    // Visual hierarchy (clearer per user feedback):
    //   * **Rings + axis ticks** = "scenery" — drawn at `ring_alpha`. When one
    //     axis is hot (hovered/dragged), all *other* axes' ring + line + handle
    //     are multiplied by `ring_dim_factor` so the focused ring pops.
    //   * **Handles** = "interactive" — drawn at `handle_alpha` (always more
    //     opaque than the rings) so they look like the actual click targets.
    //   * **Active handle** = the dragged or last-clicked axis — uses
    //     `vivid_color()` (fully saturated) and a slightly larger blob so the
    //     user can pinpoint which axis they are turning.
    let radius = rig.gizmo_radius_m.max(0.01);
    let dragging = rig.dragging_axis;
    let any_hot_axis = dragging.or(rig.hovered_axis);
    let euler = debug
        .pose_controller
        .bone_euler
        .get(bone)
        .copied()
        .unwrap_or([0.0, 0.0, 0.0]);
    let ring_alpha = rig.ring_alpha.clamp(0.0, 1.0);
    let handle_alpha = rig.handle_alpha.clamp(0.0, 1.0);
    let dim = rig.ring_dim_factor.clamp(0.0, 1.0);
    for axis in [RigEditAxis::X, RigEditAxis::Y, RigEditAxis::Z] {
        let is_hot = dragging == Some(axis) || rig.hovered_axis == Some(axis);
        let is_active = dragging == Some(axis)
            || (dragging.is_none() && rig.hovered_axis.is_none() && rig.active_axis == axis);
        let attention_factor = match any_hot_axis {
            Some(_) if !is_hot => dim,
            _ => 1.0,
        };
        let base = axis.base_color();
        let hot = axis.hot_color();
        let vivid = axis.vivid_color();

        // Ring + axis tick: dimmed scenery using base color (or hot when this
        // is the focused axis), then attenuated for non-focused axes.
        let scenery_col = if is_hot { hot } else { base };
        let ring_col = fade_color(scenery_col, ring_alpha * attention_factor);
        let q = axis_ring_rotation(r, axis);
        gizmos.circle(Isometry3d::new(p, q), radius, ring_col);
        let dir_world = r * axis.unit();
        gizmos.line(p, p + dir_world * (radius * 0.85), ring_col);

        // Drag handle: tracks the bone's current rotation around this axis,
        // so it slides around the ring as you drag it. A tangent stroke
        // shows the swing direction at the handle's current position.
        let angle = axis_handle_angle(axis, euler);
        let handle_world = axis_handle_world(p, r, axis, radius, angle);
        let tangent_local = Vec3::new(-angle.sin(), angle.cos(), 0.0) * (radius * 0.22);
        let tangent_world = q * tangent_local;

        let handle_base_col = if is_active {
            vivid
        } else if is_hot {
            hot
        } else {
            base
        };
        let handle_col = fade_color(handle_base_col, handle_alpha * attention_factor);
        let handle_size = if is_active {
            0.018
        } else if is_hot {
            0.014
        } else {
            0.010
        };
        gizmos.sphere(
            Isometry3d::new(handle_world, q),
            handle_size,
            handle_col,
        );
        gizmos.line(
            handle_world - tangent_world,
            handle_world + tangent_world,
            handle_col,
        );
    }

    // Selected joint marker — camera-facing circle in the user-configurable
    // color so it's a clear "this is the bone you're editing" indicator.
    gizmos.circle(
        Isometry3d::new(p, billboard_rot(p)),
        marker_radius * 0.65,
        select_color,
    );
}
