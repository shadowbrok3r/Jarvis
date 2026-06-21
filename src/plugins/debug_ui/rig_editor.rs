//! **Rig editor** — now hosted as a tab inside the Pose Controller window.
//!
//! Provides:
//! * viewport hover (over deforming bones only),
//! * click-to-select via LMB in edit mode,
//! * RGB axis rings around the selected joint with hover-pick,
//! * axis-aware drag rotation (no longer hardcoded to local Z),
//! * VRMC spring joint / collider tuning panels.
//!
//! The tab is rendered by [`rig_tab`] from
//! [`crate::plugins::debug_ui::pose_controller::draw_pose_controller_window`].
//! The viewport interaction systems live here as well so they can update
//! [`super::DebugUiState`] without a `plugins` ↔ `debug_ui` cycle.

use std::collections::HashMap;

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, egui};
use bevy_vrm1::prelude::{
    ColliderShape, RestGlobalTransform, SpringJointProps, SpringNodeRegistry, Vrm, VrmPath,
};

use jarvis_avatar::config::Settings;
use jarvis_avatar::icons;

use crate::plugins::mirror::{MirrorChain, MirrorState, chain_bones, mirror_quat, resolve_pair};
use crate::plugins::pose_driver::{
    BoneSnapshotHandle, IndexedBones, PoseCommand, PoseCommandSender, is_vrm_humanoid_bone,
};
use crate::plugins::undo_history::UndoHistory;
use crate::plugins::rig_editor::{
    HoverSource, RigEditAxis, RigEditorState, axis_handle_angle, axis_handle_world,
    bone_bind_world_rot,
};
use crate::plugins::spring_preset;

/// Bundled `SystemParam` so the Pose Controller window can pull in the rig
/// editor's resources without blowing past Bevy's per-system parameter limit.
#[derive(SystemParam)]
pub struct RigTabSystemParam<'w, 's> {
    pub rig: ResMut<'w, RigEditorState>,
    pub mirror: ResMut<'w, MirrorState>,
    pub vrm_q: Query<
        'w,
        's,
        (
            &'static VrmPath,
            &'static Name,
            Option<&'static SpringNodeRegistry>,
        ),
        With<Vrm>,
    >,
    pub springs: Query<'w, 's, (Entity, Option<&'static Name>, &'static mut SpringJointProps)>,
    pub colliders: Query<'w, 's, (Entity, Option<&'static Name>, &'static mut ColliderShape)>,
}

/// Pose Controller "Rig" tab — viewport / select / spring panels in one body.
///
/// Reusing the existing `pc.bone_euler` map and shared `apply_euler` path means
/// edits made via the in-world axis rings stay in sync with the Bones tab
/// sliders (and DEF-toe yaw cosmetic still applies via
/// `send_apply_bones_euler_deg`).
pub fn rig_tab(
    ui: &mut egui::Ui,
    pc: &mut super::pose_controller::PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    indexed: Option<&IndexedBones>,
    snapshot: Option<&BoneSnapshotHandle>,
    rig_params: &mut RigTabSystemParam,
) {
    // Disjoint field borrows so the closure captured by `ScrollArea` can also
    // see `springs`/`colliders` without a re-borrow conflict on `rig_params`.
    // Spring / collider panels were moved to the Bones + Expressions tab
    // (rendered below the expression presets), so the rig tab only needs
    // the rig + mirror state now.
    let RigTabSystemParam {
        rig,
        mirror,
        vrm_q: _,
        springs: _,
        colliders: _,
    } = rig_params;
    let rig: &mut RigEditorState = rig;
    let mirror: &mut MirrorState = mirror;

    ui.horizontal(|ui| {
        ui.toggle_value(&mut rig.edit_mode, "Edit mode")
            .on_hover_text(format!(
                "Master toggle for viewport hover and axis-ring drag.\n\
                 - LMB on an axis ring {arrow} rotate around that axis.\n\
                 - RMB on a bone {arrow} select it (LMB never selects a bone).\n\
                 - Off = standard orbit / pan / zoom only.",
                arrow = icons::ARROW_RIGHT
            ));
        ui.separator();
        ui.toggle_value(&mut rig.twist_drag_enabled, "Drag-rotate")
            .on_hover_text(
                "When on (and a bone is selected), LMB-drag on an axis ring rotates \
                 the bone around that ring's axis. Alt+LMB anywhere uses the active \
                 axis below as a fallback.",
            );
        ui.separator();
        ui.toggle_value(&mut rig.invert_drag_direction, "Invert drag")
            .on_hover_text(
                "When on, dragging right rotates negatively (so the visible bone \
                 follows the cursor in the opposite convention). Use this if drag \
                 feels reversed for the way you read the rings.",
            );
        ui.separator();
        ui.label("Axis:");
        for axis in [RigEditAxis::X, RigEditAxis::Y, RigEditAxis::Z] {
            let selected = rig.active_axis == axis;
            let label = egui::RichText::new(axis.label()).color(srgb_to_egui(axis.base_color()));
            if ui.selectable_label(selected, label).clicked() {
                rig.active_axis = axis;
            }
        }
    });
    ui.small(
        "Mesh hover / RMB-pick is limited to VRM humanoid bones. Extra deform \
         bones (e.g. DEF-toes, ribbon-twist) can still be picked manually from \
         the Bones tab list. Axis rings are color-coded: X red, Y green, Z \
         blue — and they're drawn in the bone's effective rotation frame \
         (parent_world · parent_rest_world⁻¹) so dragging always rotates \
         around the ring you see, even on arms / hands / fingers whose parent \
         bind isn't identity.",
    );
    ui.small(
        "Only the colored handles on each ring are interactive — drag a \
         handle to rotate around its axis. Each handle slides around its \
         ring as you rotate, showing accumulated rotation visually.",
    );

    ui.add(egui::Slider::new(&mut rig.pick_radius_m, 0.02..=0.40).text("bone pick radius (m)"));
    ui.add(
        egui::Slider::new(&mut rig.gizmo_radius_m, 0.02..=0.30).text("axis ring radius (m)"),
    );
    ui.add(
        egui::Slider::new(&mut rig.axis_pick_radius_px, 6.0..=48.0)
            .text("axis handle pick radius (px)"),
    );
    ui.add(
        egui::Slider::new(&mut rig.twist_drag_sensitivity, 0.05..=1.5)
            .text("drag sensitivity (°/px)"),
    );

    ui.collapsing("Bone pick circle", |ui| {
        ui.small(
            "Camera-facing circle drawn at hovered / selected bone joints. \
             Radius and opacity affect both hover and selection markers.",
        );
        ui.add(
            egui::Slider::new(&mut rig.bone_pick_marker_radius_m, 0.005..=0.080)
                .text("circle radius (m)"),
        );
        ui.add(
            egui::Slider::new(&mut rig.bone_pick_marker_alpha, 0.0..=1.0).text("opacity"),
        );
        ui.horizontal(|ui| {
            ui.label("Hover color");
            color_picker_rgb(ui, &mut rig.bone_pick_hover_color, "rig_hover_color");
        });
        ui.horizontal(|ui| {
            ui.label("Select color");
            color_picker_rgb(ui, &mut rig.bone_pick_select_color, "rig_select_color");
        });
    });

    ui.separator();
    mirror_panel(ui, pc, sender, mirror, rig, snapshot);

    ui.collapsing("Axis ring opacity & precision", |ui| {
        ui.small(
            "Visual hierarchy: ring outlines stay translucent so the colored \
             handles read as the actual click targets. When you hover one \
             axis, the other two dim to keep your focus on the active one.\n\n\
             Hold Shift while dragging in the viewport (or while editing in \
             the bone list) for precision adjustments.",
        );
        ui.add(
            egui::Slider::new(&mut rig.ring_alpha, 0.05..=1.0).text("ring outline opacity"),
        );
        ui.add(
            egui::Slider::new(&mut rig.handle_alpha, 0.05..=1.0).text("handle dot opacity"),
        );
        ui.add(
            egui::Slider::new(&mut rig.ring_dim_factor, 0.0..=1.0)
                .text("non-hovered axis dim"),
        );
        ui.add(
            egui::Slider::new(&mut rig.shift_precision_factor, 0.02..=1.0)
                .text("Shift precision factor"),
        );
    });

    if let Some(msg) = &rig.last_pick_message {
        ui.small(msg);
    }

    ui.separator();
    ui.label(egui::RichText::new("Selected bone").strong());
    match rig.selected_bone.clone() {
        None => {
            ui.label(egui::RichText::new("(none)").italics());
            ui.small("Click a bone in the 3D view (Edit mode on), or hover a row in the Bones tab.");
        }
        Some(bone) => {
            ui.horizontal(|ui| {
                ui.monospace(&bone);
                let in_index = indexed.is_some_and(|i| i.contains(&bone));
                if !in_index {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 140, 120),
                        "not in bone index — writes will be dropped",
                    );
                }
                if ui
                    .button("Focus camera")
                    .on_hover_text("Move the orbit camera focus onto this bone now.")
                    .clicked()
                {
                    rig.pending_focus_camera_to_bone = Some(bone.clone());
                }
                if ui
                    .button("Reveal in list")
                    .on_hover_text("Switch to the Bones tab and scroll this bone into view.")
                    .clicked()
                {
                    rig.pending_scroll_to_bone = Some(bone.clone());
                }
            });

            // Per-axis sliders driven from `pc.bone_euler` so any drag in the
            // viewport stays mirrored in the same euler buffer the Bones tab
            // shows.
            let mut current = pc
                .bone_euler
                .get(&bone)
                .copied()
                .unwrap_or([0.0, 0.0, 0.0]);
            let mut changed = false;
            for axis in [RigEditAxis::X, RigEditAxis::Y, RigEditAxis::Z] {
                ui.horizontal(|ui| {
                    let label = egui::RichText::new(axis.label())
                        .color(srgb_to_egui(axis.base_color()))
                        .strong();
                    ui.label(label);
                    let idx = axis.as_idx();
                    if ui
                        .add(egui::Slider::new(&mut current[idx], -180.0..=180.0).suffix("°"))
                        .changed()
                    {
                        changed = true;
                    }
                });
            }
            if changed {
                pc.bone_euler.insert(bone.clone(), current);
                if let Some(s) = sender {
                    super::pose_controller::send_apply_bones_euler_deg_mirrored(
                        s,
                        &bone,
                        current,
                        Some(mirror),
                    );
                    pc.status = Some(format!(
                        "rig editor: {} ({:.1}°, {:.1}°, {:.1}°)",
                        bone, current[0], current[1], current[2]
                    ));
                }
            }
            ui.small(
                "RGB axis rings show the rotation plane for each local axis. \
                 Hover one to pick that axis without using the toggle above.",
            );
        }
    }

}

/// Mirror controls — realtime toggle, per-bone "mirror selected to other
/// side" button, and per-chain dropdown. All actions push through the same
/// `PoseCommand::ApplyBones` event the bone-list and viewport drag use, so
/// mirrored writes share retarget / spring reset / animation-layer paths.
fn mirror_panel(
    ui: &mut egui::Ui,
    pc: &mut super::pose_controller::PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    mirror: &mut MirrorState,
    rig: &RigEditorState,
    snapshot: Option<&BoneSnapshotHandle>,
) {
    ui.horizontal(|ui| {
        ui.toggle_value(&mut mirror.realtime, "Realtime mirror")
            .on_hover_text(
                "When on, every bone-list slider drag and rig-handle rotation \
                 also writes the mirrored value to the partner bone (e.g. \
                 leftUpperArm + rightUpperArm). Rotation is reflected across \
                 the rig's sagittal plane in normalized humanoid space.",
            );

        if let Some(name) = pc.mirror_chain_status.clone() {
            ui.separator();
            ui.colored_label(
                egui::Color32::from_rgb(140, 200, 220),
                egui::RichText::new(name).small(),
            );
        }
    });

    ui.horizontal(|ui| {
        let selected = pc.bone_euler.is_empty();
        let _ = selected;
        // Per-bone mirror — operates on whichever bone the rig editor's
        // selected_bone resolves to (synced with bone list, viewport pick).
        let label = match (pc.bone_euler.is_empty(), pc.bone_search.is_empty()) {
            (true, _) => format!("Mirror selected {} partner", icons::ARROW_RIGHT),
            (false, _) => format!(
                "Mirror {} entries {} partner",
                pc.bone_euler.len(),
                icons::ARROW_RIGHT
            ),
        };
        let mirror_one = ui
            .button("Mirror current selection")
            .on_hover_text(
                "Snapshot the currently selected bone's rotation and apply \
                 the mirrored value to the partner (no realtime needed).",
            )
            .clicked();
        if mirror_one {
            mirror_one_bone(
                pc,
                sender,
                mirror,
                rig.selected_bone.as_deref(),
                snapshot,
            );
        }
        let _ = label;
    });

    egui::ComboBox::from_id_salt("rig_mirror_chain_pick")
        .width(220.0)
        .selected_text(icons::menu_item(icons::MIRROR, "Mirror chain"))
        .show_ui(ui, |ui| {
            for chain in [
                MirrorChain::LeftArm,
                MirrorChain::RightArm,
                MirrorChain::LeftLeg,
                MirrorChain::RightLeg,
                MirrorChain::LeftHand,
                MirrorChain::RightHand,
                MirrorChain::LeftSide,
                MirrorChain::RightSide,
                MirrorChain::AllPaired,
            ] {
                if ui.button(chain.menu_label()).clicked() {
                    mirror_chain_action(pc, sender, chain, snapshot);
                    pc.mirror_chain_status = Some(format!("Mirrored chain: {}", chain.label()));
                }
            }
        });
}

/// Mirror the rig editor's selected bone (or the first edited bone) onto its
/// partner, reading the source rotation from the live bone snapshot.
pub(super) fn mirror_one_bone(
    pc: &mut super::pose_controller::PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    mirror: &mut MirrorState,
    selected_bone: Option<&str>,
    snapshot: Option<&BoneSnapshotHandle>,
) {
    let bone = selected_bone
        .map(str::to_string)
        .or_else(|| {
            pc.bone_euler
                .iter()
                .find(|(_, v)| v.iter().any(|d| d.abs() > 1.0e-3))
                .map(|(k, _)| k.clone())
        });
    let Some(bone) = bone else {
        pc.status = Some("mirror: select a bone or edit one first".into());
        return;
    };
    let Some(primary_q) = bone_rotation_quat(&bone, snapshot, &pc.bone_euler) else {
        pc.status = Some(format!("mirror: no rotation for '{bone}'"));
        return;
    };
    let Some((partner, mirrored_q_arr)) =
        MirrorState::one_shot_partner(&bone, primary_q)
    else {
        pc.status = Some(format!("mirror: '{bone}' has no partner"));
        return;
    };
    let mirrored_deg = quat_arr_to_euler_deg(mirrored_q_arr);
    pc.bone_euler.insert(partner.clone(), mirrored_deg);
    if let Some(s) = sender {
        super::pose_controller::send_apply_bones_euler_deg(s, &partner, mirrored_deg);
        pc.status = Some(format!(
            "mirror: {bone} {} {partner}",
            icons::ARROW_RIGHT
        ));
    }
    let _ = mirror;
}

pub(super) fn mirror_chain_action(
    pc: &mut super::pose_controller::PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    chain: MirrorChain,
    snapshot: Option<&BoneSnapshotHandle>,
) {
    let bones = chain_bones(chain);
    let mut updates: HashMap<String, [f32; 4]> = HashMap::new();
    let mut status_count = 0usize;
    for src_bone in bones {
        let Some(primary_q) = bone_rotation_quat(src_bone, snapshot, &pc.bone_euler) else {
            continue;
        };
        let q = Quat::from_xyzw(primary_q[0], primary_q[1], primary_q[2], primary_q[3]);
        let Some((partner, mirrored)) = MirrorState::one_shot_partner(src_bone, primary_q) else {
            if matches!(resolve_pair(src_bone), super::super::mirror::MirrorPair::Same(_)) {
                let m = mirror_quat(q);
                updates.insert(src_bone.to_string(), [m.x, m.y, m.z, m.w]);
                pc.bone_euler
                    .insert(src_bone.to_string(), quat_arr_to_euler_deg([m.x, m.y, m.z, m.w]));
                status_count += 1;
            }
            continue;
        };
        pc.bone_euler.insert(partner.clone(), quat_arr_to_euler_deg(mirrored));
        updates.insert(partner, mirrored);
        status_count += 1;
    }
    if updates.is_empty() {
        pc.status = Some(format!("mirror chain: nothing to mirror in {}", chain.label()));
        return;
    }
    if let Some(s) = sender {
        s.send(PoseCommand::ApplyBones {
            bones: updates,
            preserve_omitted_bones: true,
            blend_weight: Some(1.0),
            transition_seconds: Some(0.0),
        });
        pc.status = Some(format!(
            "mirror chain {}: {} bone(s) updated",
            chain.label(),
            status_count
        ));
    }
}

/// Replicate the same DEF-toe cosmetic + Euler conversion the bone-list write
/// path uses, so mirrored bones land in the same normalized space as edits.
fn bone_euler_to_quat(bone: &str, deg: [f32; 3]) -> Quat {
    use crate::plugins::pose_driver::def_toe_big_yaw_slider_extra_deg;
    let yaw_extra = def_toe_big_yaw_slider_extra_deg(bone);
    Quat::from_euler(
        EulerRot::XYZ,
        deg[0].to_radians(),
        (deg[1] + yaw_extra).to_radians(),
        deg[2].to_radians(),
    )
}

fn quat_arr_to_euler_deg(q: [f32; 4]) -> [f32; 3] {
    let q = Quat::from_xyzw(q[0], q[1], q[2], q[3]);
    let (ex, ey, ez) = q.to_euler(EulerRot::XYZ);
    [ex.to_degrees(), ey.to_degrees(), ez.to_degrees()]
}

/// Live snapshot first; slider cache only when the bone is missing from snapshot.
fn bone_rotation_quat(
    bone: &str,
    snapshot: Option<&BoneSnapshotHandle>,
    bone_euler: &HashMap<String, [f32; 3]>,
) -> Option<[f32; 4]> {
    if let Some(snap_handle) = snapshot {
        let snap = snap_handle.0.read();
        if let Some(entry) = snap.bones.get(bone) {
            return Some(entry.rotation);
        }
    }
    bone_euler.get(bone).map(|deg| {
        let q = bone_euler_to_quat(bone, *deg);
        [q.x, q.y, q.z, q.w]
    })
}

fn srgb_to_egui(c: Color) -> egui::Color32 {
    let srgba = c.to_srgba();
    egui::Color32::from_rgb(
        (srgba.red.clamp(0.0, 1.0) * 255.0) as u8,
        (srgba.green.clamp(0.0, 1.0) * 255.0) as u8,
        (srgba.blue.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

/// Inline `[f32; 3]` sRGB color picker — egui's `color_edit_button_rgb`
/// works on a fixed `&mut [f32; 3]`. Wrapped so the rig tab can show an
/// interactive swatch + a small hex display next to a label.
fn color_picker_rgb(ui: &mut egui::Ui, color: &mut [f32; 3], _id_salt: &str) {
    ui.color_edit_button_rgb(color);
    let r = (color[0].clamp(0.0, 1.0) * 255.0) as u8;
    let g = (color[1].clamp(0.0, 1.0) * 255.0) as u8;
    let b = (color[2].clamp(0.0, 1.0) * 255.0) as u8;
    ui.monospace(format!("#{r:02x}{g:02x}{b:02x}"));
}

// ---------- Spring joint / collider sub-panels (carried over verbatim) -------

pub fn spring_panels(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    rig: &mut RigEditorState,
    vrm_q: &Query<
        (
            &VrmPath,
            &Name,
            Option<&SpringNodeRegistry>,
        ),
        With<Vrm>,
    >,
    springs: &mut Query<(Entity, Option<&Name>, &mut SpringJointProps)>,
    colliders: &mut Query<(Entity, Option<&Name>, &mut ColliderShape)>,
) {
    ui.collapsing("VRMC spring joints", |ui| {
        ui.label(
            "Per-joint solver weights from the loaded VRM. Names come from the glTF \
             node `Name` when present.",
        );

        let vrm_row = vrm_q.iter().next();
        let (logical_vrm_path, vrm_key, vrm_display_name, joint_chain_map) =
            if let Some((vrm_path, vrm_name, maybe_reg)) = vrm_row {
                let logical = spring_preset::logical_vrm_path(
                    Some(vrm_path.0.as_path()),
                    settings.avatar.model_path.as_str(),
                );
                let key = spring_preset::vrm_preset_key(&logical);
                let jp = maybe_reg
                    .map(spring_preset::joint_to_spring_chain)
                    .unwrap_or_default();
                (logical, key, vrm_name.as_str().to_string(), jp)
            } else {
                (String::new(), String::new(), String::new(), Vec::new())
            };

        ui.collapsing("Spring / collider preset (per VRM)", |ui| {
            ui.label(egui::RichText::new("VRM key (filename stem)").strong());
            ui.monospace(if vrm_key.is_empty() {
                "(no VRM entity)".into()
            } else {
                format!(
                    "{vrm_key}.toml  ← under {}",
                    spring_preset::SPRING_PRESETS_DIR
                )
            });
            ui.small(format!(
                "Logical path: {}  ·  Display name: {}",
                if logical_vrm_path.is_empty() {
                    "—"
                } else {
                    logical_vrm_path.as_str()
                },
                if vrm_display_name.is_empty() {
                    "—"
                } else {
                    vrm_display_name.as_str()
                }
            ));
            ui.checkbox(
                &mut settings.avatar.auto_load_spring_preset,
                "Auto-load matching preset on VRM init (if file exists)",
            );
            ui.small(
                "Uses FNV-1a hex over the logical VRM path — see module docs in \
                 spring_preset.rs.",
            );
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !vrm_key.is_empty(),
                        egui::Button::new("Export preset for this VRM…"),
                    )
                    .clicked()
                {
                    let path = spring_preset::default_preset_path_for_logical_path(
                        vrm_row.map(|(p, _, _)| p.0.as_path()),
                        settings.avatar.model_path.as_str(),
                    );
                    let joints: Vec<spring_preset::PresetJoint> = springs
                        .iter()
                        .filter_map(|(_, name, p)| {
                            let n = name?;
                            Some(spring_preset::PresetJoint {
                                name: n.as_str().to_string(),
                                stiffness: p.stiffness,
                                drag_force: p.drag_force,
                                gravity_power: p.gravity_power,
                                hit_radius: p.hit_radius,
                                gravity_dir: [
                                    p.gravity_dir.x,
                                    p.gravity_dir.y,
                                    p.gravity_dir.z,
                                ],
                            })
                        })
                        .collect();
                    let cols: Vec<spring_preset::PresetCollider> = colliders
                        .iter()
                        .filter_map(|(_, name, shape)| {
                            let n = name?;
                            Some(spring_preset::PresetCollider {
                                name: n.as_str().to_string(),
                                shape: spring_preset::PresetColliderShapeV1::from(shape),
                            })
                        })
                        .collect();
                    let snap = spring_preset::build_spring_preset_file(
                        vrm_key.clone(),
                        logical_vrm_path.clone(),
                        vrm_display_name.clone(),
                        joints,
                        cols,
                    );
                    match spring_preset::save_preset_file(&path, &snap) {
                        Ok(()) => {
                            rig.spring_ui.preset_status =
                                Some(format!("Exported {}", path.display()));
                        }
                        Err(e) => rig.spring_ui.preset_status = Some(e),
                    }
                }
                if ui
                    .add_enabled(
                        !vrm_key.is_empty(),
                        egui::Button::new("Import default file"),
                    )
                    .on_hover_text(format!(
                        "Load {}",
                        spring_preset::default_preset_path_for_logical_path(
                            vrm_row.map(|(p, _, _)| p.0.as_path()),
                            settings.avatar.model_path.as_str(),
                        )
                        .display()
                    ))
                    .clicked()
                {
                    let path = spring_preset::default_preset_path_for_logical_path(
                        vrm_row.map(|(p, _, _)| p.0.as_path()),
                        settings.avatar.model_path.as_str(),
                    );
                    match spring_preset::load_preset_file(&path) {
                        Ok(preset) => {
                            let (jh, jm, ch, cm) =
                                spring_preset::apply_spring_preset(&preset, springs, colliders);
                            rig.spring_ui.preset_status = Some(format!(
                                "Imported {} — joints {}/{} ok, colliders {}/{} ok",
                                path.display(),
                                jh,
                                jh + jm,
                                ch,
                                ch + cm
                            ));
                        }
                        Err(e) => rig.spring_ui.preset_status = Some(e),
                    }
                }
                if ui.button("Import from file…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("TOML preset", &["toml"])
                        .pick_file()
                    {
                        match spring_preset::load_preset_file(&path) {
                            Ok(preset) => {
                                let (jh, jm, ch, cm) = spring_preset::apply_spring_preset(
                                    &preset, springs, colliders,
                                );
                                let warn = if !vrm_key.is_empty() && preset.vrm_key != vrm_key {
                                    format!(
                                        " (preset key {} ≠ current {})",
                                        preset.vrm_key, vrm_key
                                    )
                                } else {
                                    String::new()
                                };
                                rig.spring_ui.preset_status = Some(format!(
                                    "Imported {}{} — joints {}/{} ok, colliders {}/{} ok",
                                    path.display(),
                                    warn,
                                    jh,
                                    jh + jm,
                                    ch,
                                    ch + cm
                                ));
                            }
                            Err(e) => rig.spring_ui.preset_status = Some(e),
                        }
                    }
                }
            });
            if let Some(msg) = &rig.spring_ui.preset_status {
                ui.small(egui::RichText::new(msg).italics());
            }
        });

        let joint_filter_lc = rig.spring_ui.joint_filter.to_lowercase();
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.add(
                egui::TextEdit::singleline(&mut rig.spring_ui.joint_filter)
                    .desired_width(160.0)
                    .hint_text("substring…"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Group by");
            egui::ComboBox::from_id_salt("rig_spring_joint_group_mode")
                .width(160.0)
                .selected_text(match rig.spring_ui.joint_group_mode {
                    0 => "All",
                    1 => "Bone name prefix",
                    2 => "VRMC spring chain",
                    _ => "All",
                })
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(rig.spring_ui.joint_group_mode == 0, "All")
                        .clicked()
                    {
                        rig.spring_ui.joint_group_mode = 0;
                    }
                    if ui
                        .selectable_label(
                            rig.spring_ui.joint_group_mode == 1,
                            "Bone name prefix",
                        )
                        .clicked()
                    {
                        rig.spring_ui.joint_group_mode = 1;
                    }
                    if ui
                        .selectable_label(
                            rig.spring_ui.joint_group_mode == 2,
                            "VRMC spring chain",
                        )
                        .clicked()
                    {
                        rig.spring_ui.joint_group_mode = 2;
                    }
                });
        });
        if rig.spring_ui.joint_group_mode == 1 {
            let mut prefixes: Vec<String> = springs
                .iter()
                .filter_map(|(_, n, _)| n.map(|x| spring_preset::bone_name_prefix(x.as_str())))
                .collect();
            prefixes.sort();
            prefixes.dedup();
            prefixes.insert(0, "(all)".to_string());
            if !prefixes.contains(&rig.spring_ui.joint_group_value) {
                rig.spring_ui.joint_group_value = "(all)".to_string();
            }
            ui.horizontal(|ui| {
                ui.label("Prefix");
                egui::ComboBox::from_id_salt("rig_spring_joint_prefix_pick")
                    .width(200.0)
                    .selected_text(rig.spring_ui.joint_group_value.clone())
                    .show_ui(ui, |ui| {
                        for p in &prefixes {
                            if ui
                                .selectable_value(
                                    &mut rig.spring_ui.joint_group_value,
                                    p.clone(),
                                    p,
                                )
                                .clicked()
                            {}
                        }
                    });
            });
        } else if rig.spring_ui.joint_group_mode == 2 {
            let mut chains: Vec<String> =
                joint_chain_map.iter().map(|(_, c)| c.clone()).collect();
            chains.sort();
            chains.dedup();
            chains.insert(0, "(all)".to_string());
            if !chains.contains(&rig.spring_ui.joint_group_value) {
                rig.spring_ui.joint_group_value = "(all)".to_string();
            }
            ui.horizontal(|ui| {
                ui.label("Spring");
                egui::ComboBox::from_id_salt("rig_spring_joint_chain_pick")
                    .width(200.0)
                    .selected_text(rig.spring_ui.joint_group_value.clone())
                    .show_ui(ui, |ui| {
                        for c in &chains {
                            if ui
                                .selectable_value(
                                    &mut rig.spring_ui.joint_group_value,
                                    c.clone(),
                                    c,
                                )
                                .clicked()
                            {}
                        }
                    });
            });
        }

        let mut rows: Vec<(Entity, Option<String>)> = Vec::new();
        for (e, name, _props) in springs.iter() {
            rows.push((e, name.map(|n| n.as_str().to_string())));
        }
        rows.sort_by(|a, b| {
            let la = a.1.as_deref().unwrap_or("");
            let lb = b.1.as_deref().unwrap_or("");
            la.cmp(lb)
        });
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                if rows.is_empty() {
                    ui.label("No spring joints on this model.");
                    return;
                }
                let mut shown = 0usize;
                for (entity, label) in &rows {
                    if !spring_row_visible(
                        label,
                        &joint_filter_lc,
                        rig.spring_ui.joint_group_mode,
                        &rig.spring_ui.joint_group_value,
                        &joint_chain_map,
                    ) {
                        continue;
                    }
                    shown += 1;
                    ui.group(|ui| {
                        ui.label(
                            egui::RichText::new(label.as_deref().unwrap_or("(unnamed)"))
                                .monospace(),
                        );
                        ui.small(format!("entity {entity:?}"));
                        if let Ok((_, _, mut p)) = springs.get_mut(*entity) {
                            ui.horizontal(|ui| {
                                ui.label("stiffness");
                                ui.add(egui::Slider::new(&mut p.stiffness, 0.0..=10.0));
                            });
                            ui.horizontal(|ui| {
                                ui.label("drag");
                                ui.add(egui::Slider::new(&mut p.drag_force, 0.0..=1.0));
                            });
                            ui.horizontal(|ui| {
                                ui.label("gravity power");
                                ui.add(egui::Slider::new(&mut p.gravity_power, -2.0..=4.0));
                            });
                            ui.horizontal(|ui| {
                                ui.label("hit radius");
                                ui.add(egui::Slider::new(&mut p.hit_radius, 0.0..=0.5));
                            });
                            ui.label("gravity dir (model space)");
                            ui.horizontal(|ui| {
                                ui.label("x");
                                ui.add(
                                    egui::DragValue::new(&mut p.gravity_dir.x).speed(0.02),
                                );
                                ui.label("y");
                                ui.add(
                                    egui::DragValue::new(&mut p.gravity_dir.y).speed(0.02),
                                );
                                ui.label("z");
                                ui.add(
                                    egui::DragValue::new(&mut p.gravity_dir.z).speed(0.02),
                                );
                            });
                            if ui.button("normalize gravity dir").clicked() {
                                let v = p.gravity_dir;
                                let len = v.length();
                                if len > 1e-6 {
                                    p.gravity_dir = v / len;
                                }
                            }
                        }
                    });
                }
                if shown == 0 {
                    ui.label("No joints match filter / category.");
                }
            });
    });

    ui.separator();
    ui.collapsing("VRMC spring colliders", |ui| {
        ui.label(
            "Collider shapes on spring-bone nodes (sphere / capsule). Radius scales with \
             parent node scale in the solver.",
        );

        let vrm_row = vrm_q.iter().next();
        let collider_chain_map = vrm_row
            .and_then(|(_, _, reg)| reg)
            .map(spring_preset::collider_to_spring_chain)
            .unwrap_or_default();

        let collider_filter_lc = rig.spring_ui.collider_filter.to_lowercase();
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.add(
                egui::TextEdit::singleline(&mut rig.spring_ui.collider_filter)
                    .desired_width(160.0)
                    .hint_text("substring…"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Group by");
            egui::ComboBox::from_id_salt("rig_spring_collider_group_mode")
                .width(160.0)
                .selected_text(match rig.spring_ui.collider_group_mode {
                    0 => "All",
                    1 => "Shape kind",
                    2 => "VRMC spring chain",
                    _ => "All",
                })
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(rig.spring_ui.collider_group_mode == 0, "All")
                        .clicked()
                    {
                        rig.spring_ui.collider_group_mode = 0;
                    }
                    if ui
                        .selectable_label(
                            rig.spring_ui.collider_group_mode == 1,
                            "Shape kind",
                        )
                        .clicked()
                    {
                        rig.spring_ui.collider_group_mode = 1;
                    }
                    if ui
                        .selectable_label(
                            rig.spring_ui.collider_group_mode == 2,
                            "VRMC spring chain",
                        )
                        .clicked()
                    {
                        rig.spring_ui.collider_group_mode = 2;
                    }
                });
        });
        if rig.spring_ui.collider_group_mode == 1 {
            let kinds = ["(all)", "Sphere", "Capsule"];
            if !kinds.contains(&rig.spring_ui.collider_group_value.as_str()) {
                rig.spring_ui.collider_group_value = "(all)".to_string();
            }
            ui.horizontal(|ui| {
                ui.label("Shape");
                egui::ComboBox::from_id_salt("rig_spring_collider_shape_pick")
                    .width(120.0)
                    .selected_text(rig.spring_ui.collider_group_value.clone())
                    .show_ui(ui, |ui| {
                        for k in kinds {
                            if ui
                                .selectable_value(
                                    &mut rig.spring_ui.collider_group_value,
                                    k.to_string(),
                                    k,
                                )
                                .clicked()
                            {}
                        }
                    });
            });
        } else if rig.spring_ui.collider_group_mode == 2 {
            let mut chains: Vec<String> =
                collider_chain_map.iter().map(|(_, c)| c.clone()).collect();
            chains.sort();
            chains.dedup();
            chains.insert(0, "(all)".to_string());
            if !chains.contains(&rig.spring_ui.collider_group_value) {
                rig.spring_ui.collider_group_value = "(all)".to_string();
            }
            ui.horizontal(|ui| {
                ui.label("Spring");
                egui::ComboBox::from_id_salt("rig_spring_collider_chain_pick")
                    .width(200.0)
                    .selected_text(rig.spring_ui.collider_group_value.clone())
                    .show_ui(ui, |ui| {
                        for c in &chains {
                            if ui
                                .selectable_value(
                                    &mut rig.spring_ui.collider_group_value,
                                    c.clone(),
                                    c,
                                )
                                .clicked()
                            {}
                        }
                    });
            });
        }

        let mut rows: Vec<(Entity, Option<String>, ColliderShape)> = Vec::new();
        for (e, name, shape) in colliders.iter() {
            rows.push((e, name.map(|n| n.as_str().to_string()), *shape));
        }
        rows.sort_by(|a, b| {
            let la = a.1.as_deref().unwrap_or("");
            let lb = b.1.as_deref().unwrap_or("");
            la.cmp(lb)
        });
        egui::ScrollArea::vertical()
            .max_height(200.0)
            .show(ui, |ui| {
                if rows.is_empty() {
                    ui.label("No collider shapes on entities in the world.");
                    return;
                }
                let mut shown = 0usize;
                for (entity, label, shape_snap) in &rows {
                    if !collider_row_visible(
                        label,
                        &collider_filter_lc,
                        rig.spring_ui.collider_group_mode,
                        &rig.spring_ui.collider_group_value,
                        &collider_chain_map,
                        shape_snap,
                    ) {
                        continue;
                    }
                    shown += 1;
                    ui.group(|ui| {
                        ui.label(
                            egui::RichText::new(label.as_deref().unwrap_or("(unnamed)"))
                                .monospace(),
                        );
                        ui.small(format!("entity {entity:?}"));
                        if let Ok((_, _, mut shape)) = colliders.get_mut(*entity) {
                            match &mut *shape {
                                ColliderShape::Sphere(sphere) => {
                                    ui.label(egui::RichText::new("Sphere").strong());
                                    ui.horizontal(|ui| {
                                        ui.label("offset");
                                        ui.add(
                                            egui::DragValue::new(&mut sphere.offset[0])
                                                .speed(0.002)
                                                .prefix("x "),
                                        );
                                        ui.add(
                                            egui::DragValue::new(&mut sphere.offset[1])
                                                .speed(0.002)
                                                .prefix("y "),
                                        );
                                        ui.add(
                                            egui::DragValue::new(&mut sphere.offset[2])
                                                .speed(0.002)
                                                .prefix("z "),
                                        );
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("radius");
                                        ui.add(egui::Slider::new(
                                            &mut sphere.radius,
                                            0.0..=0.35,
                                        ));
                                    });
                                }
                                ColliderShape::Capsule(capsule) => {
                                    ui.label(egui::RichText::new("Capsule").strong());
                                    ui.horizontal(|ui| {
                                        ui.label("offset");
                                        ui.add(
                                            egui::DragValue::new(&mut capsule.offset[0])
                                                .speed(0.002)
                                                .prefix("x "),
                                        );
                                        ui.add(
                                            egui::DragValue::new(&mut capsule.offset[1])
                                                .speed(0.002)
                                                .prefix("y "),
                                        );
                                        ui.add(
                                            egui::DragValue::new(&mut capsule.offset[2])
                                                .speed(0.002)
                                                .prefix("z "),
                                        );
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("tail");
                                        ui.add(
                                            egui::DragValue::new(&mut capsule.tail[0])
                                                .speed(0.002)
                                                .prefix("x "),
                                        );
                                        ui.add(
                                            egui::DragValue::new(&mut capsule.tail[1])
                                                .speed(0.002)
                                                .prefix("y "),
                                        );
                                        ui.add(
                                            egui::DragValue::new(&mut capsule.tail[2])
                                                .speed(0.002)
                                                .prefix("z "),
                                        );
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("radius");
                                        ui.add(egui::Slider::new(
                                            &mut capsule.radius,
                                            0.0..=0.35,
                                        ));
                                    });
                                }
                            }
                        }
                    });
                }
                if shown == 0 {
                    ui.label("No colliders match filter / category.");
                }
            });
    });
}

fn spring_row_visible(
    label: &Option<String>,
    filter_lc: &str,
    group_mode: u8,
    group_val: &str,
    joint_chain: &[(String, String)],
) -> bool {
    let label_s = label.as_deref().unwrap_or("");
    if !filter_lc.is_empty() && !label_s.to_lowercase().contains(filter_lc) {
        return false;
    }
    match group_mode {
        0 => true,
        1 => {
            group_val.is_empty()
                || group_val == "(all)"
                || spring_preset::bone_name_prefix(label_s) == group_val
        }
        2 => {
            group_val.is_empty()
                || group_val == "(all)"
                || joint_chain
                    .iter()
                    .find(|(j, _)| j == label_s)
                    .map(|(_, c)| c.as_str())
                    == Some(group_val)
        }
        _ => true,
    }
}

fn collider_row_visible(
    label: &Option<String>,
    filter_lc: &str,
    group_mode: u8,
    group_val: &str,
    collider_chain: &[(String, String)],
    shape: &ColliderShape,
) -> bool {
    let label_s = label.as_deref().unwrap_or("");
    if !filter_lc.is_empty() && !label_s.to_lowercase().contains(filter_lc) {
        return false;
    }
    match group_mode {
        0 => true,
        1 => {
            group_val.is_empty()
                || group_val == "(all)"
                || match shape {
                    ColliderShape::Sphere(_) => group_val == "Sphere",
                    ColliderShape::Capsule(_) => group_val == "Capsule",
                }
        }
        2 => {
            group_val.is_empty()
                || group_val == "(all)"
                || collider_chain
                    .iter()
                    .find(|(j, _)| j == label_s)
                    .map(|(_, c)| c.as_str())
                    == Some(group_val)
        }
        _ => true,
    }
}

// ---------- Viewport pick / hover / axis-drag ----------------------------------

fn ray_closest_point(ray: &Ray3d, point: Vec3) -> Vec3 {
    let d = ray.direction.as_vec3();
    let t = (point - ray.origin).dot(d).clamp(0.0, 1.0e6);
    ray.origin + d * t
}

fn dist_ray_point(ray: &Ray3d, point: Vec3) -> f32 {
    let closest = ray_closest_point(ray, point);
    point.distance(closest)
}

/// Minimum distance from `ray` to the line segment *ab* (sampled for cheap UX picking).
fn dist_ray_segment(ray: &Ray3d, a: Vec3, b: Vec3) -> f32 {
    let mut m = dist_ray_point(ray, a).min(dist_ray_point(ray, b));
    const STEPS: u32 = 10;
    for i in 1..STEPS {
        let t = i as f32 / STEPS as f32;
        let p = a.lerp(b, t);
        m = m.min(dist_ray_point(ray, p));
    }
    m
}

fn seed_bone_euler_from_snapshot(
    bone: &str,
    snapshot: Option<&BoneSnapshotHandle>,
    bone_euler: &mut HashMap<String, [f32; 3]>,
) {
    let Some(snap) = snapshot else {
        bone_euler
            .entry(bone.to_string())
            .or_insert([0.0, 0.0, 0.0]);
        return;
    };
    let snap = snap.0.read();
    if let Some(entry) = snap.bones.get(bone) {
        let q = Quat::from_xyzw(
            entry.rotation[0],
            entry.rotation[1],
            entry.rotation[2],
            entry.rotation[3],
        );
        let (ex, ey, ez) = q.to_euler(EulerRot::XYZ);
        bone_euler.insert(
            bone.to_string(),
            [ex.to_degrees(), ey.to_degrees(), ez.to_degrees()],
        );
    } else {
        bone_euler
            .entry(bone.to_string())
            .or_insert([0.0, 0.0, 0.0]);
    }
}

/// Project a world-space point through the camera and return its viewport
/// (screen) coordinates, if it's in front of the camera.
fn world_to_viewport(cam: &Camera, cam_gt: &GlobalTransform, p: Vec3) -> Option<Vec2> {
    cam.world_to_viewport(cam_gt, p).ok()
}

/// Pixel distance from `cursor_px` to the projected position of an axis
/// drag handle. Only the handle (a single point on the ring at
/// `axis_handle_angle(axis, current_euler_deg)`) is interactive — the ring
/// outline is purely visual feedback. This gives clear "click here to grab"
/// semantics and prevents the user from accidentally rotating a bone by
/// brushing past the ring outline.
fn axis_handle_pixel_distance(
    cam: &Camera,
    cam_gt: &GlobalTransform,
    centre: Vec3,
    frame_rot: Quat,
    axis: RigEditAxis,
    radius: f32,
    current_euler_deg: [f32; 3],
    cursor_px: Vec2,
) -> Option<f32> {
    let angle = axis_handle_angle(axis, current_euler_deg);
    let world = axis_handle_world(centre, frame_rot, axis, radius, angle);
    let proj = world_to_viewport(cam, cam_gt, world)?;
    Some(proj.distance(cursor_px))
}

/// Continuous viewport hover system — runs every frame in edit mode (when egui
/// does not want the pointer). Updates [`RigEditorState::hovered_bone`] and
/// [`RigEditorState::hovered_axis`] so the gizmo and Pose Controller hint can
/// react.
pub(crate) fn rig_editor_viewport_hover(
    mut contexts: EguiContexts,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cam_q: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    child_of: Query<&ChildOf>,
    gtf_q: Query<&GlobalTransform>,
    rest_gtf_q: Query<&RestGlobalTransform>,
    indexed: Option<Res<IndexedBones>>,
    debug: Res<super::DebugUiState>,
    mut rig: ResMut<RigEditorState>,
) {
    if !rig.edit_mode {
        // Stale hover state would otherwise keep highlighting bones while the
        // user is no longer in edit mode.
        if rig.hovered_source == HoverSource::Viewport {
            rig.hovered_bone = None;
            rig.hovered_source = HoverSource::None;
            rig.hovered_axis = None;
        }
        return;
    }
    // Don't fight an active drag — the drag-axis path uses motion deltas only.
    if rig.dragging_axis.is_some() {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    if ctx.egui_wants_pointer_input() {
        if rig.hovered_source == HoverSource::Viewport {
            rig.hovered_bone = None;
            rig.hovered_source = HoverSource::None;
            rig.hovered_axis = None;
        }
        return;
    }
    // While the user is mid-orbit (LMB without intent to pick a bone) we
    // suppress hover updates so the highlight doesn't flicker across bones.
    if mouse.pressed(MouseButton::Left) && rig.hovered_axis.is_none() {
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
    let Some(indexed) = indexed else {
        return;
    };
    if indexed.is_empty() {
        return;
    }

    // 1) Axis HANDLE hover (only when a bone is selected). The rings are
    // drawn in the bone's effective rotation frame (see `bone_bind_world_rot`)
    // and the handles slide around them as the bone is rotated, so hover
    // hit-testing must use the same frame + the bone's current Euler.
    let mut new_axis: Option<RigEditAxis> = None;
    if let Some(sel) = rig.selected_bone.as_deref()
        && let Some(entity) = indexed.entity(sel)
        && let Ok(gtf) = gtf_q.get(entity)
    {
        let centre = gtf.translation();
        let frame_rot = bone_bind_world_rot(entity, &gtf_q, &rest_gtf_q, &child_of);
        let euler = debug
            .pose_controller
            .bone_euler
            .get(sel)
            .copied()
            .unwrap_or([0.0, 0.0, 0.0]);
        let max_px = rig.axis_pick_radius_px.max(1.0);
        let mut best: Option<(RigEditAxis, f32)> = None;
        for axis in [RigEditAxis::X, RigEditAxis::Y, RigEditAxis::Z] {
            let Some(d) = axis_handle_pixel_distance(
                cam,
                cam_gt,
                centre,
                frame_rot,
                axis,
                rig.gizmo_radius_m,
                euler,
                cursor,
            ) else {
                continue;
            };
            if d > max_px {
                continue;
            }
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((axis, d));
            }
        }
        new_axis = best.map(|(a, _)| a);
    }
    if rig.hovered_axis != new_axis {
        rig.hovered_axis = new_axis;
    }

    // If the cursor is over an axis ring, the bone hover should stay on the
    // currently selected bone (so the highlight doesn't shift to a neighbor),
    // and we MUST short-circuit before the bone-pick block — otherwise an
    // off-axis bone behind the ring could steal hover and prime a stray pick
    // for the next click.
    if rig.hovered_axis.is_some() {
        return;
    }

    // 2) Bone hover (mesh raycast against humanoid bones only — extras like
    // DEF-toes or twist bones can still be selected from the Bones tab list).
    let r = rig.pick_radius_m.max(0.02);
    let mut best: Option<(f32, String)> = None;
    for (name, entity) in &indexed.entities {
        if !is_vrm_humanoid_bone(name) {
            continue;
        }
        let Ok(gtf) = gtf_q.get(*entity) else {
            continue;
        };
        let p = gtf.translation();
        let mut d = dist_ray_point(&ray, p);
        if let Ok(co) = child_of.get(*entity) {
            if let Ok(parent_gtf) = gtf_q.get(co.parent()) {
                let p0 = parent_gtf.translation();
                d = d.min(dist_ray_segment(&ray, p0, p));
            }
        }
        if d > r {
            continue;
        }
        if best.as_ref().map(|(bd, _)| d < *bd).unwrap_or(true) {
            best = Some((d, name.clone()));
        }
    }
    let new_hover = best.map(|(_, n)| n);
    if rig.hovered_bone != new_hover {
        rig.hovered_bone = new_hover;
        rig.hovered_source = if rig.hovered_bone.is_some() {
            HoverSource::Viewport
        } else {
            HoverSource::None
        };
    }
}

/// Edit-mode click handling.
///
/// * LMB on an axis ring → start a sticky axis-drag (caught by `axis_drag`).
/// * LMB anywhere else → no bone selection (we deliberately reserve LMB for
///   axis rings + orbit), so the user can never miss a ring and accidentally
///   reselect a different finger.
/// * RMB → pick the nearest **humanoid** bone under the cursor. Extras (e.g.
///   DEF-toes, twist bones) are list-only — see the Bones tab for those.
/// * Camera focus is **not** triggered here. List-click in edit mode is the
///   only path that snaps the orbit camera (see `pose_controller::bone_row`).
pub(crate) fn rig_editor_viewport_pick(
    mut contexts: EguiContexts,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cam_q: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    child_of: Query<&ChildOf>,
    gtf_q: Query<&GlobalTransform>,
    indexed: Option<Res<IndexedBones>>,
    snapshot: Option<Res<BoneSnapshotHandle>>,
    mut rig: ResMut<RigEditorState>,
    mut debug: ResMut<super::DebugUiState>,
) {
    if !rig.edit_mode {
        return;
    }

    // Sticky drag start: if the user just pressed LMB while an axis was
    // hovered, capture the drag for the rest of this LMB press. Always
    // returns early so the LMB never falls through to bone selection.
    if mouse.just_pressed(MouseButton::Left) {
        if let Some(axis) = rig.hovered_axis {
            rig.dragging_axis = Some(axis);
            rig.active_axis = axis;
        }
        return;
    }
    if mouse.just_released(MouseButton::Left) {
        rig.dragging_axis = None;
        return;
    }

    // RMB is the only way to pick a bone from the viewport. Anything else
    // (LMB on empty space, drags, releases) falls through above.
    if !mouse.just_pressed(MouseButton::Right) {
        return;
    }
    // If a ring is under the cursor, never let the click also select a bone
    // behind it — same anti-snag rule as during LMB axis grabs.
    if rig.hovered_axis.is_some() {
        return;
    }

    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    if ctx.egui_wants_pointer_input() {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        rig.last_pick_message = Some("no cursor position".into());
        return;
    };
    let Ok((cam, cam_gt)) = cam_q.single() else {
        rig.last_pick_message = Some("no camera".into());
        return;
    };
    let Ok(ray) = cam.viewport_to_world(cam_gt, cursor) else {
        rig.last_pick_message = Some("viewport_to_world failed".into());
        return;
    };
    let Some(indexed) = indexed else {
        rig.last_pick_message = Some("bone index not ready".into());
        return;
    };
    if indexed.is_empty() {
        rig.last_pick_message = Some("no indexed bones".into());
        return;
    }

    let r = rig.pick_radius_m.max(0.02);
    let mut best: Option<(f32, String)> = None;
    for (name, entity) in &indexed.entities {
        // Mesh-pick is humanoid-only by user request — extras stay list-only.
        if !is_vrm_humanoid_bone(name) {
            continue;
        }
        let Ok(gtf) = gtf_q.get(*entity) else {
            continue;
        };
        let p = gtf.translation();
        let mut d = dist_ray_point(&ray, p);
        if let Ok(co) = child_of.get(*entity) {
            let parent = co.parent();
            if let Ok(parent_gtf) = gtf_q.get(parent) {
                let p0 = parent_gtf.translation();
                d = d.min(dist_ray_segment(&ray, p0, p));
            }
        }
        if d > r {
            continue;
        }
        let replace = best.as_ref().map(|(bd, _)| d < *bd).unwrap_or(true);
        if replace {
            best = Some((d, name.clone()));
        }
    }

    match best {
        Some((d, name)) => {
            rig.selected_bone = Some(name.clone());
            seed_bone_euler_from_snapshot(
                &name,
                snapshot.as_deref(),
                &mut debug.pose_controller.bone_euler,
            );
            rig.last_pick_message = Some(format!("picked '{name}' (ray dist {d:.3} m)"));
            rig.pending_scroll_to_bone = Some(name);
            // NOTE: deliberately *no* `pending_focus_camera_to_bone` here — the
            // user asked that viewport picks never snap the camera; only list
            // clicks (in edit mode) do.
        }
        None => {
            rig.last_pick_message = Some(format!(
                "no humanoid bone within {:.2} m of pointer — increase pick radius?",
                r
            ));
        }
    }
}

/// LMB drag (with or without Alt) on the selected bone rotates around the
/// active / dragged axis.
///
/// **Why axis-angle composition instead of `euler[axis] += delta`:** the
/// pose driver applies bone rotation as `Quat::from_euler(XYZ, x, y, z)`,
/// which is intrinsic XYZ. That means once the bone has any prior rotation,
/// adding to a single euler component no longer produces a clean rotation
/// around the *visible* ring's axis — the X rotation happens before Y and
/// Z, so the Y/Z axes drift relative to the gizmo. We instead compose in
/// quaternion space and re-decompose to XYZ.
///
/// **Why the drag axis is `rest_world · local_axis`, not just `local_axis`:**
/// the pose driver's effective math is `bone_world = M · L · R_w` where
/// `M = parent_world · parent_rest_world⁻¹`, `L = pose_q`, and
/// `R_w = bone's rest-world rotation`. The ring is drawn perpendicular to
/// `bone_world · X` (so it visibly hugs the bone), and we want the drag to
/// rotate around exactly that visible axis. Solving
/// `M · L_new · R_w = R(bone_world·X, θ) · M · L · R_w` gives
/// `L_new = L · R(R_w · X, θ)`. So we post-multiply by an axis pre-rotated
/// through `R_w`. When `R_w = I` (spine, root), this collapses to the
/// naive `current_q * R(X, θ)` and shoulders work; for arms/fingers
/// (where the bone's rest world basis is rotated relative to the parent),
/// the `R_w` factor is what kills the cyclic-axis-swap.
pub(crate) fn rig_editor_axis_drag(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    accum_motion: Res<AccumulatedMouseMotion>,
    sender: Option<Res<PoseCommandSender>>,
    rig: Res<RigEditorState>,
    mirror: Res<MirrorState>,
    indexed: Option<Res<IndexedBones>>,
    rest_gtf_q: Query<&RestGlobalTransform>,
    snapshot: Option<Res<BoneSnapshotHandle>>,
    undo: Res<UndoHistory>,
    mut drag_checkpoint_done: Local<bool>,
    mut debug: ResMut<super::DebugUiState>,
) {
    // Reset the per-drag checkpoint guard whenever LMB is released, so the
    // next drag can record a fresh undo entry.
    if !mouse.pressed(MouseButton::Left) {
        *drag_checkpoint_done = false;
    }
    if !rig.edit_mode || !rig.twist_drag_enabled {
        return;
    }
    let Some(bone) = rig.selected_bone.clone() else {
        return;
    };
    if !mouse.pressed(MouseButton::Left) {
        return;
    }
    let alt_held = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let axis = match rig.dragging_axis {
        Some(a) => a,
        None if alt_held => rig.active_axis,
        None => return,
    };
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    if ctx.egui_wants_pointer_input() {
        return;
    }
    let Some(sender) = sender.as_deref() else {
        return;
    };

    let dx = accum_motion.delta.x;
    if dx.abs() < f32::EPSILON {
        return;
    }

    // First write of this LMB-drag — snapshot the pre-drag rig so Ctrl-Z
    // jumps back to where the drag started, not to every per-pixel frame.
    if !*drag_checkpoint_done {
        if let Some(snap) = snapshot.as_deref() {
            undo.record(snap, &debug.pose_controller.bone_euler, format!("drag {bone}"));
        }
        *drag_checkpoint_done = true;
    }
    let dir = if rig.invert_drag_direction { -1.0 } else { 1.0 };
    // Shift = Blender-style precision modifier. Multiplying degrees-per-pixel
    // by `shift_precision_factor` (default 0.15) gives ≈7× finer control
    // without forcing the user to crank the global sensitivity slider down.
    let shift_held = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let precision = if shift_held {
        rig.shift_precision_factor.clamp(0.01, 1.0)
    } else {
        1.0
    };
    let delta_deg = dx * rig.twist_drag_sensitivity * dir * precision;

    // The drag axis must be rotated through the bone's rest-world rotation so
    // that post-multiplying with `current_q * R(R_w·axis, θ)` produces a
    // world-space rotation around the visible ring's normal `bone_world·axis`.
    // Without this factor, arms/fingers/hands (whose rest_world is rotated
    // relative to their parent) drag along the wrong ring's path.
    let rest_world_rot = indexed
        .as_deref()
        .and_then(|idx| idx.entity(&bone))
        .and_then(|e| rest_gtf_q.get(e).ok())
        .map(|r| r.0.rotation())
        .unwrap_or(Quat::IDENTITY);

    let euler = debug
        .pose_controller
        .bone_euler
        .entry(bone.clone())
        .or_insert([0.0, 0.0, 0.0]);

    let current_q = Quat::from_euler(
        EulerRot::XYZ,
        euler[0].to_radians(),
        euler[1].to_radians(),
        euler[2].to_radians(),
    );
    let drag_axis_local = (rest_world_rot * axis.unit()).normalize_or_zero();
    let drag_axis_local = if drag_axis_local == Vec3::ZERO {
        axis.unit()
    } else {
        drag_axis_local
    };
    let delta_q = Quat::from_axis_angle(drag_axis_local, delta_deg.to_radians());
    let new_q = (current_q * delta_q).normalize();
    let (nx, ny, nz) = new_q.to_euler(EulerRot::XYZ);
    euler[0] = nx.to_degrees();
    euler[1] = ny.to_degrees();
    euler[2] = nz.to_degrees();

    let e = *euler;
    super::pose_controller::send_apply_bones_euler_deg_mirrored(sender, &bone, e, Some(&mirror));
    debug.pose_controller.status = Some(format!(
        "rig editor (drag {}{}): {} → ({:.1}°, {:.1}°, {:.1}°) Δ{} {:+.1}°",
        if rig.invert_drag_direction { "inv" } else { "fwd" },
        if mirror.realtime { "+mir" } else { "" },
        bone,
        e[0],
        e[1],
        e[2],
        axis.label(),
        delta_deg,
    ));
}
