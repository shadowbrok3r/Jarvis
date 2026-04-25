//! [`RigEditorState`] (shared UI + pick settings) and in-world axis gizmo lines
//! for the selected bone.
//!
//! Viewport **pick** and **Alt+LMB twist** systems live in
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
            .insert_gizmo_config(RigEditorGizmoGroup::default(), rig_editor_gizmo_config())
            .add_systems(
                PostUpdate,
                rig_editor_draw_gizmo.after(TransformSystems::Propagate),
            );
    }
}

#[derive(Resource)]
pub struct RigEditorState {
    /// When set, right-clicks in the 3D view run a bone ray-pick (if egui
    /// does not want the pointer).
    pub viewport_pick_enabled: bool,
    /// Alt + LMB drag nudges the selected bone's local Z euler (degrees).
    pub twist_drag_enabled: bool,
    pub selected_bone: Option<String>,
    pub pick_radius_m: f32,
    pub twist_drag_sensitivity: f32,
    pub last_pick_message: Option<String>,
}

impl Default for RigEditorState {
    fn default() -> Self {
        Self {
            viewport_pick_enabled: false,
            twist_drag_enabled: true,
            selected_bone: None,
            pick_radius_m: 0.14,
            twist_drag_sensitivity: 0.35,
            last_pick_message: None,
        }
    }
}

fn rig_editor_draw_gizmo(
    mut gizmos: Gizmos<RigEditorGizmoGroup>,
    rig: Res<RigEditorState>,
    indexed: Option<Res<IndexedBones>>,
    gtf_q: Query<&GlobalTransform>,
) {
    let Some(bone) = rig.selected_bone.as_deref() else {
        return;
    };
    let Some(indexed) = indexed else {
        return;
    };
    let Some(entity) = indexed.entity(bone) else {
        return;
    };
    let Ok(gtf) = gtf_q.get(entity) else {
        return;
    };
    let p = gtf.translation();
    let r = gtf.rotation();
    let axis_len = 0.12_f32;
    let axes = [
        (Vec3::X, Color::srgb(0.95, 0.25, 0.25)),
        (Vec3::Y, Color::srgb(0.35, 0.9, 0.35)),
        (Vec3::Z, Color::srgb(0.35, 0.45, 0.95)),
    ];
    for (dir, col) in axes {
        let w = r * dir * axis_len;
        gizmos.line(p, p + w, col);
    }
    // Joint “head” — makes the pick target easier to see in dense rigs.
    gizmos.sphere(Isometry3d::new(p, r), 0.012, Color::srgb(0.92, 0.92, 0.98));
}
