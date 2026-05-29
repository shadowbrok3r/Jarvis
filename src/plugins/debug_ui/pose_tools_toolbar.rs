//! Global "Pose Tools" toolbar — a single horizontal strip docked under the
//! application menu bar that exposes the rig-editor, mirror, and panel-
//! visibility controls regardless of which Pose Controller tab (or panel
//! side) the user is currently viewing.
//!
//! Why pull these out of the per-tab UI?
//! * **Edit mode + axis selector** were buried inside the Rig tab. The user
//!   commonly wants to flip into edit mode while their Bones / Library tab
//!   is on screen, without first navigating to Rig.
//! * **Mirror controls** (realtime toggle + chain dropdown) used to live in
//!   the Rig tab's side panel. The same chain operations apply when the
//!   user is in Bones / Library, so we hoist them up.
//! * **Per-panel show/hide** lets users collapse a side / bottom panel
//!   without going through the View menu — handy when the workspace gets
//!   crowded.
//!
//! The toolbar only renders when `settings.ui.show_pose_controller` is true
//! (the same gate that controls the rest of the Pose Controller surface),
//! and it's positioned via `egui::TopBottomPanel::top` so it sits between
//! the menu bar and the rig hover-hint strip drawn by
//! [`super::pose_controller::draw_pose_controller_window`].

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

use jarvis_avatar::config::Settings;
use jarvis_avatar::icons;

use crate::plugins::mirror::{MirrorChain, MirrorState};
use crate::plugins::pose_driver::PoseCommandSender;
use crate::plugins::rig_editor::{RigEditAxis, RigEditorState};

use super::pose_controller::{PoseControllerTab, PoseControllerUiState};
use super::rig_editor::{mirror_chain_action, mirror_one_bone};

/// Render the global Pose Tools toolbar — see module docs.
pub fn draw_pose_tools_toolbar(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<super::DebugUiState>,
    mut rig: ResMut<RigEditorState>,
    mut mirror: ResMut<MirrorState>,
    sender: Option<Res<PoseCommandSender>>,
) {
    if !settings.ui.show_pose_controller {
        return;
    }
    if settings.ui.pose_tools_toolbar_pos == "none" {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    egui::TopBottomPanel::top("pose_tools_global_toolbar").show(ctx, |ui| {
        ui.horizontal_wrapped(|ui| {
            edit_mode_section(ui, &mut rig);
            ui.separator();
            axis_section(ui, &mut rig);
            ui.separator();
            mirror_section(ui, &mut state.pose_controller, &mut mirror, sender.as_deref());
            ui.separator();
            panel_visibility_section(ui, &mut settings, &state.pose_controller);
        });
    });
}

fn edit_mode_section(ui: &mut egui::Ui, rig: &mut RigEditorState) {
    ui.menu_button("Edit", |ui| {
        ui.toggle_value(&mut rig.edit_mode, "Edit mode")
            .on_hover_text(
                "Master toggle for viewport hover and axis-ring drag.\n\
                - LMB on an axis ring -> rotate around that axis.\n\
                - RMB on a bone -> select it (LMB never selects a bone).\n\
                - Off = standard orbit / pan / zoom only.",
            );
        ui.toggle_value(&mut rig.twist_drag_enabled, "Drag")
            .on_hover_text(
                "When on (and a bone is selected), LMB-drag on an axis ring \
                rotates the bone around that ring's axis.",
            );
        ui.toggle_value(&mut rig.invert_drag_direction, "Invert drag")
            .on_hover_text(
                "Flip the drag direction so the visible bone follows the cursor \
                in the opposite convention.",
            );
    });
}

fn axis_section(ui: &mut egui::Ui, rig: &mut RigEditorState) {
    for axis in [RigEditAxis::X, RigEditAxis::Y, RigEditAxis::Z] {
        let selected = rig.active_axis == axis;
        let label = egui::RichText::new(axis.label())
            .color(srgb_to_egui(axis.base_color()))
            .strong();
        if ui
            .selectable_label(selected, label)
            .on_hover_text(format!(
                "Make {} the active axis for handle hover-pick fallback and Alt+LMB drag.",
                axis.label()
            ))
            .clicked()
        {
            rig.active_axis = axis;
        }
    }
}

fn mirror_section(
    ui: &mut egui::Ui,
    pc: &mut PoseControllerUiState,
    mirror: &mut MirrorState,
    sender: Option<&PoseCommandSender>,
) {
    ui.toggle_value(&mut mirror.realtime, "Realtime mirror")
        .on_hover_text(
            "When on, every bone-list slider drag and rig-handle rotation \
             also writes the mirrored value to the partner bone.",
        );
    if ui
        .button("Mirror selected")
        .on_hover_text(
            "Snapshot the currently selected bone's rotation and apply the \
             mirrored value to the partner.",
        )
        .clicked()
    {
        mirror_one_bone(pc, sender, mirror);
    }
    egui::ComboBox::from_id_salt("global_mirror_chain_pick")
        .width(180.0)
        .selected_text("Mirror chain")
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
                if ui.button(chain.label()).clicked() {
                    mirror_chain_action(pc, sender, chain);
                    pc.mirror_chain_status = Some(format!("Mirrored chain: {}", chain.label()));
                }
            }
        });
    if let Some(name) = &pc.mirror_chain_status {
        ui.colored_label(
            egui::Color32::from_rgb(140, 200, 220),
            egui::RichText::new(name).small(),
        );
    }
}

/// Per-panel show/hide row. Each tab gets a small toggle button — clicking
/// flips it between "hidden" and the workspace default side. The Animation
/// Layers panel toggle is included alongside since it follows the same
/// "dock-or-hide" pattern.
fn panel_visibility_section(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    pc: &PoseControllerUiState,
) {
    ui.menu_button("Panels", |ui| {
        let default_side = settings.ui.pose_controller_dock_side.clone();
        for tab in PoseControllerTab::all() {
            let key = tab.config_key().to_string();
            let current = settings
                .ui
                .pose_controller_tab_dock_sides
                .get(&key)
                .cloned()
                .unwrap_or_else(|| default_side.clone());
            let visible = current != "hidden";
            let label = format!("{}{}", tab.label(), side_glyph_for(&current));
            let resp = ui
                .selectable_label(visible, label)
                .on_hover_text(format!(
                    "{} {} side. Click to {}",
                    tab.label(),
                    if visible { current.as_str() } else { "hidden" },
                    if visible { "hide" } else { "show" }
                ));
            if resp.clicked() {
                if visible {
                    settings
                        .ui
                        .pose_controller_tab_dock_sides
                        .insert(key, "hidden".to_string());
                } else {
                    // Restore to default workspace side.
                    settings.ui.pose_controller_tab_dock_sides.remove(&key);
                }
            }
        }
    });

    // Animation Layers — owned by `anim_layers::draw_anim_layers_window`,
    // but its show/hide is global, so we place it here next to the pose
    // panels for parity.
    ui.separator();
    let anim_visible = settings.ui.show_anim_layers;
    let anim_label = if anim_visible {
        format!("Anim Layers {}", anim_layers_side_glyph(&settings.ui.anim_layers_dock_side))
    } else {
        "Anim Layers (off)".to_string()
    };
    let anim_resp = ui
        .selectable_label(anim_visible, anim_label)
        .on_hover_text(
            "Show / hide the Animation Layers panel (dopesheet at the bottom \
             by default; right-click to switch its dock side).",
        );
    if anim_resp.clicked() {
        settings.ui.show_anim_layers = !anim_visible;
    }
    anim_resp.context_menu(|ui| {
        ui.label(egui::RichText::new("Animation Layers panel").strong());
        ui.separator();
        let mut button = |ui: &mut egui::Ui, label: String, target: &str| {
            let active = settings.ui.anim_layers_dock_side == target;
            if ui
                .add_enabled(!active, egui::Button::new(label))
                .clicked()
            {
                settings.ui.anim_layers_dock_side = target.to_string();
                settings.ui.show_anim_layers = true;
                ui.close();
            }
        };
        button(ui, format!("{} Bottom panel (dopesheet)", icons::DOCK_BOTTOM), "bottom");
        button(ui, format!("{} Left side panel", icons::DOCK_LEFT), "left");
        button(ui, format!("{} Right side panel", icons::DOCK_RIGHT), "right");
        button(ui, format!("{} Floating window", icons::FLOATING), "floating");
    });

    // Quick read-out of the per-side active-tab map so users can see what's
    // currently focused without opening every panel.
    let mut summary_parts: Vec<String> = Vec::new();
    for side in ["left", "right", "bottom", "floating"] {
        if let Some(tab) = pc.per_side_active.get(side) {
            summary_parts.push(format!("{side}={}", tab.label()));
        }
    }
    if !summary_parts.is_empty() {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(summary_parts.join(" · "))
                    .small()
                    .color(egui::Color32::from_gray(150)),
            );
        });
    }
}

fn side_glyph_for(side: &str) -> &'static str {
    match side {
        "left" => " [L]",
        "right" => " [R]",
        "bottom" => " [B]",
        "floating" => " [F]",
        "hidden" => " (off)",
        _ => "",
    }
}

fn anim_layers_side_glyph(side: &str) -> &'static str {
    match side {
        "left" => "[L]",
        "right" => "[R]",
        "bottom" => "[B]",
        "floating" => "[F]",
        _ => "",
    }
}

fn srgb_to_egui(c: Color) -> egui::Color32 {
    let srgba = c.to_srgba();
    egui::Color32::from_rgb(
        (srgba.red.clamp(0.0, 1.0) * 255.0) as u8,
        (srgba.green.clamp(0.0, 1.0) * 255.0) as u8,
        (srgba.blue.clamp(0.0, 1.0) * 255.0) as u8,
    )
}
