//! **Rig editor** window: viewport bone picking + euler controls, axis gizmo
//! (drawn in-world by [`crate::plugins::rig_editor`]), and VRMC spring joint
//! tuning (`SpringJointProps`).

use std::collections::HashMap;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts};
use bevy_vrm1::prelude::{ColliderShape, SpringJointProps, SpringNodeRegistry, Vrm, VrmPath};

use jarvis_avatar::config::Settings;

use crate::plugins::pose_driver::{
    BoneSnapshotHandle, IndexedBones, PoseCommand, PoseCommandSender,
};
use crate::plugins::rig_editor::RigEditorState;
use crate::plugins::spring_preset;

pub fn draw_rig_editor_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut rig: ResMut<RigEditorState>,
    mut debug: ResMut<super::DebugUiState>,
    indexed: Option<Res<IndexedBones>>,
    sender: Option<Res<PoseCommandSender>>,
    vrm_q: Query<(&VrmPath, &Name, Option<&SpringNodeRegistry>), With<Vrm>>,
    mut springs: Query<(Entity, Option<&Name>, &mut SpringJointProps)>,
    mut colliders: Query<(Entity, Option<&Name>, &mut ColliderShape)>,
) {
    if !settings.ui.show_rig_editor {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let mut open = settings.ui.show_rig_editor;
    egui::Window::new("Rig editor")
        .default_size([420.0, 520.0])
        .resizable(true)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new("Viewport").strong());
            ui.checkbox(
                &mut rig.viewport_pick_enabled,
                "Right-click picks bone (3D view, when egui is not using the pointer)",
            );
            ui.checkbox(
                &mut rig.twist_drag_enabled,
                "Alt + LMB drag twists selected bone (local Z, degrees)",
            );
            ui.small(
                "While Alt+LMB is active in the 3D view, orbit (LMB drag) is paused so the viewport does not spin.",
            );
            ui.add(egui::Slider::new(&mut rig.pick_radius_m, 0.03..=0.45).text("pick radius (m)"));
            ui.add(
                egui::Slider::new(&mut rig.twist_drag_sensitivity, 0.05..=1.5)
                    .text("twist drag sensitivity"),
            );
            if let Some(msg) = &rig.last_pick_message {
                ui.small(msg);
            }
            ui.separator();

            ui.label(egui::RichText::new("Selected bone").strong());
            match &rig.selected_bone {
                None => {
                    ui.label(egui::RichText::new("(none)").italics());
                }
                Some(bone) => {
                    ui.monospace(bone);
                    let in_index = indexed.as_ref().is_some_and(|i| i.contains(bone));
                    if !in_index {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 140, 120),
                            "not in bone index — writes may be ignored",
                        );
                    }
                    let mut x = debug
                        .pose_controller
                        .bone_euler
                        .get(bone)
                        .map(|e| e[0])
                        .unwrap_or(0.0);
                    let mut y = debug
                        .pose_controller
                        .bone_euler
                        .get(bone)
                        .map(|e| e[1])
                        .unwrap_or(0.0);
                    let mut z = debug
                        .pose_controller
                        .bone_euler
                        .get(bone)
                        .map(|e| e[2])
                        .unwrap_or(0.0);
                    ui.horizontal(|ui| {
                        ui.label("X");
                        if ui
                            .add(egui::Slider::new(&mut x, -180.0..=180.0).suffix("°"))
                            .changed()
                        {
                            let e = {
                                let entry = debug
                                    .pose_controller
                                    .bone_euler
                                    .entry(bone.clone())
                                    .or_insert([0.0, 0.0, 0.0]);
                                entry[0] = x;
                                *entry
                            };
                            if let Some(s) = sender.as_deref() {
                                apply_euler(bone, e, s, &mut debug.pose_controller.status);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Y");
                        if ui
                            .add(egui::Slider::new(&mut y, -180.0..=180.0).suffix("°"))
                            .changed()
                        {
                            let e = {
                                let entry = debug
                                    .pose_controller
                                    .bone_euler
                                    .entry(bone.clone())
                                    .or_insert([0.0, 0.0, 0.0]);
                                entry[1] = y;
                                *entry
                            };
                            if let Some(s) = sender.as_deref() {
                                apply_euler(bone, e, s, &mut debug.pose_controller.status);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Z");
                        if ui
                            .add(egui::Slider::new(&mut z, -180.0..=180.0).suffix("°"))
                            .changed()
                        {
                            let e = {
                                let entry = debug
                                    .pose_controller
                                    .bone_euler
                                    .entry(bone.clone())
                                    .or_insert([0.0, 0.0, 0.0]);
                                entry[2] = z;
                                *entry
                            };
                            if let Some(s) = sender.as_deref() {
                                apply_euler(bone, e, s, &mut debug.pose_controller.status);
                            }
                        }
                    });
                    ui.small("RGB axis lines are drawn at this joint in the 3D view.");
                }
            }

            ui.separator();
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
                        format!("{vrm_key}.toml  ← under {}", spring_preset::SPRING_PRESETS_DIR)
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
                                        gravity_dir: [p.gravity_dir.x, p.gravity_dir.y, p.gravity_dir.z],
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
                            .add_enabled(!vrm_key.is_empty(), egui::Button::new("Import default file"))
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
                                    let (jh, jm, ch, cm) = spring_preset::apply_spring_preset(
                                        &preset,
                                        &mut springs,
                                        &mut colliders,
                                    );
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
                                            &preset,
                                            &mut springs,
                                            &mut colliders,
                                        );
                                        let warn = if !vrm_key.is_empty() && preset.vrm_key != vrm_key
                                        {
                                            format!(" (preset key {} ≠ current {})", preset.vrm_key, vrm_key)
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
                            if ui.selectable_label(rig.spring_ui.joint_group_mode == 0, "All").clicked()
                            {
                                rig.spring_ui.joint_group_mode = 0;
                            }
                            if ui
                                .selectable_label(rig.spring_ui.joint_group_mode == 1, "Bone name prefix")
                                .clicked()
                            {
                                rig.spring_ui.joint_group_mode = 1;
                            }
                            if ui
                                .selectable_label(rig.spring_ui.joint_group_mode == 2, "VRMC spring chain")
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
                    let mut chains: Vec<String> = joint_chain_map
                        .iter()
                        .map(|(_, c)| c.clone())
                        .collect();
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
                                        ui.add(egui::DragValue::new(&mut p.gravity_dir.x).speed(0.02));
                                        ui.label("y");
                                        ui.add(egui::DragValue::new(&mut p.gravity_dir.y).speed(0.02));
                                        ui.label("z");
                                        ui.add(egui::DragValue::new(&mut p.gravity_dir.z).speed(0.02));
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
                                .selectable_label(rig.spring_ui.collider_group_mode == 1, "Shape kind")
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
                    let mut chains: Vec<String> = collider_chain_map
                        .iter()
                        .map(|(_, c)| c.clone())
                        .collect();
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
        });
    settings.ui.show_rig_editor = open;
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

fn apply_euler(
    bone: &str,
    euler_deg: [f32; 3],
    sender: &PoseCommandSender,
    status: &mut Option<String>,
) {
    let q = Quat::from_euler(
        EulerRot::XYZ,
        euler_deg[0].to_radians(),
        euler_deg[1].to_radians(),
        euler_deg[2].to_radians(),
    );
    let mut bones = HashMap::new();
    bones.insert(bone.to_string(), [q.x, q.y, q.z, q.w]);
    sender.send(PoseCommand::ApplyBones {
        bones,
        preserve_omitted_bones: true,
        blend_weight: Some(1.0),
        transition_seconds: Some(0.0),
    });
    *status = Some(format!(
        "rig editor: {} ({:.1}°, {:.1}°, {:.1}°)",
        bone, euler_deg[0], euler_deg[1], euler_deg[2]
    ));
}

// ---------- Viewport pick + twist (live here to touch DebugUiState) ----------

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
        bone_euler.entry(bone.to_string()).or_insert([0.0, 0.0, 0.0]);
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
        bone_euler.entry(bone.to_string()).or_insert([0.0, 0.0, 0.0]);
    }
}

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
    if !rig.viewport_pick_enabled {
        return;
    }
    if !mouse.just_pressed(MouseButton::Right) {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    if ctx.wants_pointer_input() {
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
        let replace = best
            .as_ref()
            .map(|(bd, _)| d < *bd)
            .unwrap_or(true);
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
        }
        None => {
            rig.last_pick_message = Some(format!(
                "no bone within {:.2} m of pointer — increase pick radius?",
                r
            ));
        }
    }
}

pub(crate) fn rig_editor_alt_drag_twist(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    accum_motion: Res<AccumulatedMouseMotion>,
    sender: Option<Res<PoseCommandSender>>,
    rig: Res<RigEditorState>,
    mut debug: ResMut<super::DebugUiState>,
) {
    if !rig.twist_drag_enabled {
        return;
    }
    let Some(bone) = rig.selected_bone.clone() else {
        return;
    };
    if !keys.pressed(KeyCode::AltLeft) && !keys.pressed(KeyCode::AltRight) {
        return;
    }
    if !mouse.pressed(MouseButton::Left) {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    if ctx.wants_pointer_input() {
        return;
    }
    let Some(sender) = sender.as_deref() else {
        return;
    };

    let dx = accum_motion.delta.x;
    if dx.abs() < f32::EPSILON {
        return;
    }

    let euler = debug
        .pose_controller
        .bone_euler
        .entry(bone.clone())
        .or_insert([0.0, 0.0, 0.0]);
    euler[2] += dx * rig.twist_drag_sensitivity;
    let e = *euler;
    apply_euler(&bone, e, sender, &mut debug.pose_controller.status);
}
