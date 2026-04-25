//! "Animation Layers" debug window.
//!
//! Shows every entry in the [`LayerStack`](crate::plugins::anim_layers::LayerStack)
//! as a row with:
//!   * enable checkbox
//!   * kind + label (inline editable)
//!   * weight slider
//!   * play / pause / rewind / delete
//!   * blend-mode dropdown
//!   * horizontal timeline with a sweeping playhead marker
//!   * (expanded) per-driver parameter editors
//!
//! Pose-hold layers pin a saved [`PoseFile`] (bones + expression weights)
//! from the pose library — stack two with masks for start/end style posing.
//!
//! A footer button lets the user install the default "procedural" stack
//! (breathing + auto-blink + weight-shift + finger / toe fidget), or add
//! any individual driver from the dropdown.
//!
//! The window surface is a thin Bevy system that locks the
//! [`LayerStackHandle`](crate::plugins::anim_layers::LayerStackHandle) and
//! drives everything through `handle.with_write` — so the UI and the ECS
//! system never race.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use jarvis_avatar::config::Settings;
use jarvis_avatar::pose_library::{slugify, AnimationFile};

use crate::plugins::anim_layer_sets::LayerSetsStore;
use crate::plugins::anim_layers::{
    BlendMode, BoneMask, DriverKind, Layer, LayerStack, LayerStackHandle, RestPoseSnapshot,
};
use crate::plugins::pose_driver::{IndexedBones, VRM_BONE_NAMES};
use crate::plugins::pose_library_assets::PoseLibraryAssets;

/// Transient per-window state kept on `DebugUiState`. Holds nothing that
/// would be worth persisting across launches — just scratch for dropdowns.
#[derive(Default)]
pub struct AnimLayersUiState {
    pub add_kind: AddDriverChoice,
    pub status: Option<String>,
    pub picked_clip: String,
    /// Display [`PoseFile::name`] for "Pose from library…".
    pub picked_pose: String,
    pub expanded: std::collections::HashSet<u64>,
    /// Selected layer set name in the save/load dropdown.
    pub picked_set: String,
    /// Scratch buffer for the "Save as…" text input.
    pub new_set_name: String,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum AddDriverChoice {
    #[default]
    Breathing,
    AutoBlink,
    WeightShift,
    FingerFidget,
    ToeFidget,
    ClipFromLibrary,
    PoseFromLibrary,
}

impl AddDriverChoice {
    fn label(self) -> &'static str {
        match self {
            Self::Breathing => "Breathing",
            Self::AutoBlink => "Auto-Blink",
            Self::WeightShift => "Weight Shift",
            Self::FingerFidget => "Finger Fidget",
            Self::ToeFidget => "Toe Fidget",
            Self::ClipFromLibrary => "Clip from Library…",
            Self::PoseFromLibrary => "Pose from Library…",
        }
    }
}

const ALL_CHOICES: &[AddDriverChoice] = &[
    AddDriverChoice::Breathing,
    AddDriverChoice::AutoBlink,
    AddDriverChoice::WeightShift,
    AddDriverChoice::FingerFidget,
    AddDriverChoice::ToeFidget,
    AddDriverChoice::ClipFromLibrary,
    AddDriverChoice::PoseFromLibrary,
];

pub fn draw_anim_layers_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<super::DebugUiState>,
    handle: Option<Res<LayerStackHandle>>,
    library: Option<Res<PoseLibraryAssets>>,
    rest: Option<Res<RestPoseSnapshot>>,
    indexed: Option<Res<IndexedBones>>,
    layer_sets: Option<Res<LayerSetsStore>>,
) {
    if !settings.ui.show_anim_layers {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else { return };
    let Some(handle) = handle else { return };

    let available_bones = available_bone_names(indexed.as_deref());

    let mut open = settings.ui.show_anim_layers;
    egui::Window::new("Animation Layers")
        .default_size([680.0, 520.0])
        .min_width(540.0)
        .resizable(true)
        .open(&mut open)
        .show(ctx, |ui| {
            handle.with_write(|stack| {
                top_bar(ui, &mut state.anim_layers, stack, rest.as_deref());
                ui.separator();
                layer_sets_bar(
                    ui,
                    &mut state.anim_layers,
                    stack,
                    layer_sets.as_deref(),
                    library.as_deref(),
                );
                ui.separator();
                layer_list(ui, &mut state.anim_layers, stack, &available_bones);
                ui.separator();
                add_layer_bar(
                    ui,
                    &mut state.anim_layers,
                    stack,
                    library.as_deref(),
                );
                if let Some(msg) = &state.anim_layers.status {
                    ui.add_space(4.0);
                    ui.colored_label(egui::Color32::from_rgb(160, 200, 160), msg);
                }
                if let Some(store) = layer_sets.as_deref() {
                    let guard = store.inner.read();
                    if let Some(err) = &guard.last_error {
                        ui.colored_label(egui::Color32::from_rgb(220, 120, 120), err);
                    }
                }
            });
        });
    settings.ui.show_anim_layers = open;
}

/// Build a sorted, de-duplicated bone list for dropdowns. Prefers the
/// bones actually present on the currently loaded VRM (via
/// [`IndexedBones`]) so the list stays accurate across models; falls
/// back to the canonical [`VRM_BONE_NAMES`] humanoid set for bootstrap
/// frames before a model loads.
fn available_bone_names(indexed: Option<&IndexedBones>) -> Vec<String> {
    let mut names: Vec<String> = match indexed {
        Some(i) if !i.is_empty() => i.names.iter().cloned().collect(),
        _ => VRM_BONE_NAMES.iter().map(|s| (*s).to_string()).collect(),
    };
    names.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
    names.dedup();
    names
}

// ---------------------------------------------------------------------------

fn top_bar(
    ui: &mut egui::Ui,
    ui_state: &mut AnimLayersUiState,
    stack: &mut LayerStack,
    rest: Option<&RestPoseSnapshot>,
) {
    ui.horizontal(|ui| {
        ui.checkbox(&mut stack.master_enabled, "Master enabled")
            .on_hover_text(
                "When off, the stack is a no-op — the rig is fully driven by \
                 manual MCP / idle VRMA / slider writes. Flip this on to run \
                 every layer below.",
            );
        ui.separator();
        if ui
            .button("▶ Play all")
            .on_hover_text("Set every layer's playing flag to true")
            .clicked()
        {
            for layer in &mut stack.layers {
                layer.playing = true;
            }
            ui_state.status = Some("all layers playing".into());
        }
        if ui
            .button("⏸ Pause all")
            .on_hover_text("Set every layer's playing flag to false")
            .clicked()
        {
            for layer in &mut stack.layers {
                layer.playing = false;
            }
            ui_state.status = Some("all layers paused".into());
        }
        ui.separator();
        ui.label(egui::RichText::new(format!("{} layer(s)", stack.layers.len())).monospace());
        ui.separator();
        let bones = rest.map(|r| r.captured).unwrap_or(0);
        ui.label(
            egui::RichText::new(format!("{bones} rest bones"))
                .monospace()
                .color(if bones == 0 {
                    egui::Color32::from_rgb(200, 150, 120)
                } else {
                    egui::Color32::from_rgb(140, 200, 140)
                }),
        )
        .on_hover_text(
            "How many bones the layer-stack has captured rest rotations for. \
             Procedural layers need this populated before they produce \
             anything sensible — if it's stuck at 0 the pose driver's bone \
             index isn't settled yet (give it a second after VRM load).",
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("t = {:.2}s", stack.clock))
                    .monospace()
                    .color(egui::Color32::from_gray(150)),
            );
        });
    });
}

fn layer_sets_bar(
    ui: &mut egui::Ui,
    ui_state: &mut AnimLayersUiState,
    stack: &mut LayerStack,
    store: Option<&LayerSetsStore>,
    library: Option<&PoseLibraryAssets>,
) {
    let Some(store) = store else {
        ui.small("layer sets unavailable");
        return;
    };
    let names = store.sorted_names();
    ui.horizontal(|ui| {
        ui.label("Set:");
        egui::ComboBox::from_id_salt("anim_layer_set_pick")
            .selected_text(if ui_state.picked_set.is_empty() {
                "(pick a saved set)"
            } else {
                ui_state.picked_set.as_str()
            })
            .width(220.0)
            .show_ui(ui, |ui| {
                for n in &names {
                    ui.selectable_value(&mut ui_state.picked_set, n.clone(), n);
                }
            });
        let has_pick = !ui_state.picked_set.is_empty();
        if ui
            .add_enabled(has_pick, egui::Button::new("↺ Load"))
            .on_hover_text("Replace the current stack with the selected set")
            .clicked()
        {
            if let Some(lib) = library {
                match store.load_into(&ui_state.picked_set, stack, &lib.library) {
                    Ok(count) => {
                        ui_state.status =
                            Some(format!("loaded '{}' ({count} layers)", ui_state.picked_set));
                    }
                    Err(e) => ui_state.status = Some(e),
                }
            } else {
                ui_state.status = Some("pose library not ready".into());
            }
        }
        if ui
            .add_enabled(has_pick, egui::Button::new("🗑 Delete"))
            .on_hover_text("Remove the selected set (save to persist)")
            .clicked()
        {
            let name = ui_state.picked_set.clone();
            store.delete(&name);
            ui_state.picked_set.clear();
            ui_state.status = Some(format!("deleted '{name}'"));
        }
        ui.separator();
        ui.label("Save as:");
        ui.add(
            egui::TextEdit::singleline(&mut ui_state.new_set_name)
                .hint_text("e.g. idle-relaxed")
                .desired_width(180.0),
        );
        let can_save = !ui_state.new_set_name.trim().is_empty();
        if ui
            .add_enabled(can_save, egui::Button::new("💾 Save current"))
            .on_hover_text("Snapshot every layer above into this set")
            .clicked()
        {
            let name = ui_state.new_set_name.trim().to_string();
            store.save_current(&name, stack);
            ui_state.picked_set = name.clone();
            ui_state.new_set_name.clear();
            ui_state.status = Some(format!("saved '{name}' (click Persist to disk)"));
        }
        if ui
            .button("↧ Persist")
            .on_hover_text("Flush all saved sets to config/anim_layer_sets.json")
            .clicked()
        {
            store.persist();
            let msg = store
                .inner
                .read()
                .last_status
                .clone()
                .unwrap_or_else(|| "persisted".into());
            ui_state.status = Some(msg);
        }
        if ui.button("⟳ Reload").on_hover_text("Drop in-memory sets and re-read from disk").clicked() {
            store.reload();
            ui_state.status = Some("reloaded from disk".into());
        }
    });
}

fn layer_list(
    ui: &mut egui::Ui,
    ui_state: &mut AnimLayersUiState,
    stack: &mut LayerStack,
    bone_names: &[String],
) {
    let mut to_remove: Option<u64> = None;
    let mut to_move: Option<(usize, isize)> = None;

    egui::ScrollArea::vertical()
        .id_salt("anim_layers_list")
        .auto_shrink([false, false])
        .max_height(340.0)
        .show(ui, |ui| {
            if stack.layers.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    ui.label(egui::RichText::new("No layers yet").italics());
                    ui.small("Use + Add or Install defaults.");
                });
                return;
            }
            for (idx, layer) in stack.layers.iter_mut().enumerate() {
                let action = layer_row(ui, ui_state, idx, layer, bone_names);
                match action {
                    Some(LayerAction::Delete) => to_remove = Some(layer.id),
                    Some(LayerAction::MoveUp) => to_move = Some((idx, -1)),
                    Some(LayerAction::MoveDown) => to_move = Some((idx, 1)),
                    None => {}
                }
            }
        });

    if let Some(id) = to_remove {
        if stack.remove_layer(id) {
            ui_state.status = Some(format!("deleted layer {id}"));
        }
    }
    if let Some((idx, delta)) = to_move {
        let target = idx as isize + delta;
        if target >= 0 && (target as usize) < stack.layers.len() {
            stack.move_layer(idx, target as usize);
        }
    }
}

enum LayerAction {
    Delete,
    MoveUp,
    MoveDown,
}

fn layer_row(
    ui: &mut egui::Ui,
    ui_state: &mut AnimLayersUiState,
    idx: usize,
    layer: &mut Layer,
    bone_names: &[String],
) -> Option<LayerAction> {
    let mut action: Option<LayerAction> = None;
    let header_color = kind_color(layer.driver.kind_label());
    let frame_color = if layer.enabled {
        egui::Color32::from_rgba_unmultiplied(header_color.r(), header_color.g(), header_color.b(), 28)
    } else {
        egui::Color32::from_gray(30)
    };
    let expanded = ui_state.expanded.contains(&layer.id);

    egui::Frame::group(ui.style())
        .fill(frame_color)
        .show(ui, |ui| {
            // Row 1: enable | kind | label | weight | transport | delete
            ui.horizontal(|ui| {
                ui.checkbox(&mut layer.enabled, "");
                ui.colored_label(header_color, format!("[{}]", layer.driver.kind_label()));
                ui.add(egui::TextEdit::singleline(&mut layer.label).desired_width(140.0));
                ui.separator();
                ui.label("wgt");
                ui.add(
                    egui::Slider::new(&mut layer.weight, 0.0..=1.0)
                        .fixed_decimals(2)
                        .show_value(true),
                );
                ui.separator();
                let icon = if layer.playing { "⏸" } else { "▶" };
                if ui.button(icon).on_hover_text("play / pause").clicked() {
                    layer.playing = !layer.playing;
                }
                if ui.button("⏹").on_hover_text("rewind").clicked() {
                    layer.time = 0.0;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("✖️").on_hover_text("delete layer").clicked() {
                        action = Some(LayerAction::Delete);
                    }
                    if ui.button("▼").on_hover_text("move down").clicked() {
                        action = Some(LayerAction::MoveDown);
                    }
                    if ui.button("▲").on_hover_text("move up").clicked() {
                        action = Some(LayerAction::MoveUp);
                    }
                    let expand_icon = if expanded { "▾" } else { "▸" };
                    if ui
                        .small_button(expand_icon)
                        .on_hover_text("expand / collapse driver params")
                        .clicked()
                    {
                        if expanded {
                            ui_state.expanded.remove(&layer.id);
                        } else {
                            ui_state.expanded.insert(layer.id);
                        }
                    }
                });
            });

            // Row 2: timeline
            timeline(ui, layer);

            // Row 3 (optional): driver params.
            if ui_state.expanded.contains(&layer.id) {
                ui.add_space(4.0);
                ui.separator();
                driver_params(ui, layer);
                ui.separator();
                mask_and_blend(ui, layer, bone_names);
            }
        });
    let _ = idx;
    action
}

fn timeline(ui: &mut egui::Ui, layer: &Layer) {
    let (t, duration) = layer.timeline_progress();
    let pct = (t / duration).clamp(0.0, 1.0);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    // Track.
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(28));
    // Fill up to playhead.
    let mut fill = rect;
    fill.set_width(rect.width() * pct);
    let fill_col = kind_color(layer.driver.kind_label());
    let fill_col_dim = egui::Color32::from_rgba_unmultiplied(
        fill_col.r(),
        fill_col.g(),
        fill_col.b(),
        if layer.enabled { 160 } else { 60 },
    );
    painter.rect_filled(fill, 2.0, fill_col_dim);
    // Playhead line.
    let head_x = rect.left() + rect.width() * pct;
    painter.line_segment(
        [
            egui::pos2(head_x, rect.top()),
            egui::pos2(head_x, rect.bottom()),
        ],
        egui::Stroke::new(1.5, egui::Color32::from_rgb(240, 220, 120)),
    );
    // Time text.
    let label = if layer.duration.is_some() {
        format!("{:0.2} / {:0.2}s", t, duration)
    } else {
        format!("∞  phase {:0.2}s", t)
    };
    painter.text(
        rect.right_top() + egui::vec2(-4.0, 2.0),
        egui::Align2::RIGHT_TOP,
        label,
        egui::FontId::monospace(10.0),
        egui::Color32::from_gray(200),
    );
}

fn driver_params(ui: &mut egui::Ui, layer: &mut Layer) {
    match &mut layer.driver {
        DriverKind::Clip { animation } => {
            ui.label(egui::RichText::new(format!(
                "clip: {}  ·  {} frames @ {:.1} fps",
                animation.name,
                animation.frames.len(),
                animation.fps
            )));
            ui.horizontal(|ui| {
                ui.label("speed");
                ui.add(egui::Slider::new(&mut layer.speed, 0.0..=2.5).fixed_decimals(2));
                ui.checkbox(&mut layer.looping, "loop");
            });
        }
        DriverKind::PoseHold { pose } => {
            ui.label(egui::RichText::new(format!(
                "pose: {}  ·  {} bones  ·  {} expression(s)  ·  file `{}`",
                pose.name,
                pose.bones.len(),
                pose.expressions.len(),
                format!("{}.json", slugify(&pose.name)),
            )));
        }
        DriverKind::Breathing {
            rate_hz,
            pitch_deg,
            roll_deg,
        } => {
            slider(ui, "rate (Hz)", rate_hz, 0.05..=1.5);
            slider(ui, "pitch (°)", pitch_deg, 0.0..=4.0);
            slider(ui, "roll (°)", roll_deg, 0.0..=3.0);
        }
        DriverKind::Blink {
            mean_interval,
            double_blink_chance,
            next_in,
            phase,
            phase_t,
        } => {
            slider(ui, "mean interval (s)", mean_interval, 1.0..=10.0);
            slider(ui, "double-blink p", double_blink_chance, 0.0..=0.5);
            ui.label(
                egui::RichText::new(format!(
                    "next in: {:.2}s · phase: {phase:?} · phase-t: {:.2}s",
                    next_in, phase_t
                ))
                .small()
                .color(egui::Color32::from_gray(170)),
            );
        }
        DriverKind::WeightShift {
            rate_hz,
            hip_roll_deg,
            spine_counter_deg,
        } => {
            slider(ui, "rate (Hz)", rate_hz, 0.02..=0.5);
            slider(ui, "hip roll (°)", hip_roll_deg, 0.0..=5.0);
            slider(ui, "spine counter (°)", spine_counter_deg, 0.0..=3.0);
        }
        DriverKind::FingerFidget {
            amplitude_deg,
            frequency_hz,
            seed,
        } => {
            slider(ui, "amplitude (°)", amplitude_deg, 0.0..=6.0);
            slider(ui, "frequency (Hz)", frequency_hz, 0.05..=1.5);
            ui.horizontal(|ui| {
                ui.label("seed");
                ui.monospace(format!("{:#x}", seed));
                if ui.button("reshuffle").clicked() {
                    *seed = rand::random::<u64>();
                }
            });
        }
        DriverKind::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
        } => {
            slider(ui, "amplitude (°)", amplitude_deg, 0.0..=6.0);
            slider(ui, "frequency (Hz)", frequency_hz, 0.05..=1.5);
            ui.horizontal(|ui| {
                ui.label("seed");
                ui.monospace(format!("{:#x}", seed));
                if ui.button("reshuffle").clicked() {
                    *seed = rand::random::<u64>();
                }
            });
        }
    }
}

fn mask_and_blend(ui: &mut egui::Ui, layer: &mut Layer, bone_names: &[String]) {
    ui.horizontal(|ui| {
        ui.label("blend");
        egui::ComboBox::from_id_salt(("blend_mode", layer.id))
            .selected_text(layer.blend_mode.label())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut layer.blend_mode, BlendMode::Override, "override");
                ui.selectable_value(
                    &mut layer.blend_mode,
                    BlendMode::RestRelative,
                    "additive (rest-relative)",
                );
            });
        ui.separator();
        ui.label("speed");
        ui.add(egui::Slider::new(&mut layer.speed, 0.0..=3.0).fixed_decimals(2));
    });
    bone_mask_editor(
        ui,
        egui::Id::new(("mask_include", layer.id)),
        "include",
        &mut layer.mask.include,
        bone_names,
    );
    bone_mask_editor(
        ui,
        egui::Id::new(("mask_exclude", layer.id)),
        "exclude",
        &mut layer.mask.exclude,
        bone_names,
    );
    ui.small("Empty include = all bones.");
}

/// Chip-style bone mask editor:
/// * chips for each currently-selected bone with an × to remove
/// * `+ bone…` combo box listing every unselected bone from the VRM's
///   indexed humanoid set
/// * free-text input for bones that aren't in the humanoid set (rare,
///   but some rigs have extra twist bones etc.)
fn bone_mask_editor(
    ui: &mut egui::Ui,
    id: egui::Id,
    label: &str,
    selection: &mut Vec<String>,
    all_bones: &[String],
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("{label}:"));

        let mut to_remove: Option<usize> = None;
        for (i, bone) in selection.iter().enumerate() {
            ui.scope(|ui| {
                ui.visuals_mut().widgets.inactive.weak_bg_fill =
                    egui::Color32::from_rgb(60, 70, 90);
                let exists = all_bones.iter().any(|b| b == bone);
                let color = if exists {
                    egui::Color32::from_rgb(200, 220, 240)
                } else {
                    egui::Color32::from_rgb(220, 170, 120)
                };
                let chip = egui::Button::new(
                    egui::RichText::new(format!("{bone}  ×")).small().color(color),
                );
                if ui
                    .add(chip)
                    .on_hover_text(if exists {
                        "click to remove"
                    } else {
                        "not present in this VRM — click to remove"
                    })
                    .clicked()
                {
                    to_remove = Some(i);
                }
            });
        }
        if let Some(i) = to_remove {
            selection.remove(i);
        }

        // Add-combo: only offers bones not already selected.
        let remaining: Vec<&String> =
            all_bones.iter().filter(|b| !selection.contains(*b)).collect();
        egui::ComboBox::from_id_salt(id.with("add_combo"))
            .selected_text("+ bone…")
            .width(170.0)
            .show_ui(ui, |ui| {
                if remaining.is_empty() {
                    ui.label(egui::RichText::new("(all bones selected)").small());
                }
                for bone in remaining {
                    if ui.selectable_label(false, bone).clicked() {
                        selection.push(bone.clone());
                    }
                }
            });

        // Manual input for bones outside the humanoid set. We stash the
        // scratch buffer in `egui::Memory` keyed by `id` so it survives
        // between frames without polluting `AnimLayersUiState`.
        let scratch_id = id.with("custom_input");
        let mut scratch: String = ui
            .ctx()
            .data(|d| d.get_temp::<String>(scratch_id).unwrap_or_default());
        let resp = ui.add(
            egui::TextEdit::singleline(&mut scratch)
                .hint_text("custom…")
                .desired_width(100.0),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            for name in split_csv(&scratch) {
                if !selection.contains(&name) {
                    selection.push(name);
                }
            }
            scratch.clear();
        }
        ui.ctx().data_mut(|d| d.insert_temp(scratch_id, scratch));
    });
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

fn slider<Num>(ui: &mut egui::Ui, label: &str, value: &mut Num, range: std::ops::RangeInclusive<Num>)
where
    Num: egui::emath::Numeric,
{
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::Slider::new(value, range).fixed_decimals(2));
    });
}

// ---------------------------------------------------------------------------
// Add-layer bar
// ---------------------------------------------------------------------------

fn add_layer_bar(
    ui: &mut egui::Ui,
    ui_state: &mut AnimLayersUiState,
    stack: &mut LayerStack,
    library: Option<&PoseLibraryAssets>,
) {
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("anim_layer_add_kind")
            .selected_text(ui_state.add_kind.label())
            .show_ui(ui, |ui| {
                for choice in ALL_CHOICES {
                    ui.selectable_value(&mut ui_state.add_kind, *choice, choice.label());
                }
            });

        if matches!(ui_state.add_kind, AddDriverChoice::ClipFromLibrary) {
            if let Some(library) = library {
                egui::ComboBox::from_id_salt("anim_layer_add_clip")
                    .selected_text(if ui_state.picked_clip.is_empty() {
                        "(pick a clip)"
                    } else {
                        ui_state.picked_clip.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for meta in library.animations() {
                            ui.selectable_value(
                                &mut ui_state.picked_clip,
                                meta.filename.clone(),
                                &meta.name,
                            );
                        }
                    });
            }
        }

        if matches!(ui_state.add_kind, AddDriverChoice::PoseFromLibrary) {
            if let Some(library) = library {
                egui::ComboBox::from_id_salt("anim_layer_add_pose")
                    .selected_text(if ui_state.picked_pose.is_empty() {
                        "(pick a pose)"
                    } else {
                        ui_state.picked_pose.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for pose in library.poses() {
                            ui.selectable_value(
                                &mut ui_state.picked_pose,
                                pose.name.clone(),
                                format!("{} · {}", pose.name, pose.category),
                            );
                        }
                    });
            }
        }

        if ui.button("➕ Add").clicked() {
            match try_build_layer(ui_state, library) {
                Ok(layer) => {
                    let id = stack.add_layer(layer);
                    ui_state.status = Some(format!("added layer #{id}"));
                }
                Err(msg) => ui_state.status = Some(msg),
            }
        }
        ui.separator();
        if ui
            .button("Install defaults")
            .on_hover_text(
                "Add the default procedural stack.",
            )
            .clicked()
        {
            stack.install_default_procedural_layers();
            stack.master_enabled = true;
            ui_state.status = Some("installed default procedural stack".into());
        }
        if ui.button("Clear all").clicked() {
            stack.layers.clear();
            ui_state.status = Some("cleared layers".into());
        }
    });
}

fn try_build_layer(
    ui_state: &AnimLayersUiState,
    library: Option<&PoseLibraryAssets>,
) -> Result<Layer, String> {
    let layer = match ui_state.add_kind {
        AddDriverChoice::Breathing => Layer::new(
            "breathing",
            "Breathing",
            DriverKind::breathing_default(),
        )
        .blend(BlendMode::RestRelative)
        .weight(1.0),
        AddDriverChoice::AutoBlink => Layer::new(
            "auto-blink",
            "Auto-Blink",
            DriverKind::blink_default(),
        )
        .blend(BlendMode::Override)
        .weight(1.0),
        AddDriverChoice::WeightShift => Layer::new(
            "weight-shift",
            "Weight Shift",
            DriverKind::weight_shift_default(),
        )
        .blend(BlendMode::RestRelative)
        .weight(0.8),
        AddDriverChoice::FingerFidget => Layer::new(
            "finger-fidget",
            "Finger Fidget",
            DriverKind::finger_fidget_default(),
        )
        .blend(BlendMode::RestRelative)
        .weight(0.6),
        AddDriverChoice::ToeFidget => Layer::new(
            "toe-fidget",
            "Toe Fidget",
            DriverKind::toe_fidget_default(),
        )
        .blend(BlendMode::RestRelative)
        .weight(0.4),
        AddDriverChoice::ClipFromLibrary => {
            let library = library.ok_or("pose library not ready")?;
            if ui_state.picked_clip.is_empty() {
                return Err("pick a clip first".into());
            }
            let animation: AnimationFile = library
                .library
                .load_animation(&ui_state.picked_clip)
                .map_err(|e| format!("load_animation({}): {e}", ui_state.picked_clip))?;
            let name = animation.name.clone();
            Layer::new(name.clone(), name, DriverKind::Clip { animation: Box::new(animation) })
                .blend(BlendMode::Override)
                .weight(1.0)
        }
        AddDriverChoice::PoseFromLibrary => {
            let library = library.ok_or("pose library not ready")?;
            if ui_state.picked_pose.is_empty() {
                return Err("pick a pose first".into());
            }
            let pose_file = library
                .poses()
                .into_iter()
                .find(|p| p.name == ui_state.picked_pose)
                .ok_or_else(|| format!("pose '{}' not in library", ui_state.picked_pose))?;
            let slug = slugify(&pose_file.name);
            let label = pose_file.name.clone();
            Layer::new(
                slug,
                label,
                DriverKind::PoseHold {
                    pose: Box::new(pose_file),
                },
            )
            .blend(BlendMode::Override)
            .weight(1.0)
        }
    };
    // Mirror the per-kind blend default into a new struct with a fresh mask.
    Ok(Layer {
        mask: BoneMask::default(),
        ..layer
    })
}

// ---------------------------------------------------------------------------
// Palette helpers
// ---------------------------------------------------------------------------

fn kind_color(kind: &str) -> egui::Color32 {
    match kind {
        "clip" => egui::Color32::from_rgb(120, 190, 255),
        "pose-hold" => egui::Color32::from_rgb(200, 160, 255),
        "breathing" => egui::Color32::from_rgb(160, 220, 160),
        "auto-blink" => egui::Color32::from_rgb(220, 200, 130),
        "weight-shift" => egui::Color32::from_rgb(210, 150, 220),
        "finger-fidget" => egui::Color32::from_rgb(220, 160, 140),
        "toe-fidget" => egui::Color32::from_rgb(140, 200, 220),
        _ => egui::Color32::from_gray(200),
    }
}
