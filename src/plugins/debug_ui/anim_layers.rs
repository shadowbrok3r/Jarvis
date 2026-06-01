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

use std::collections::{HashMap, HashSet};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_egui::{
    egui,
    egui::containers::menu::{MenuButton, MenuConfig},
    egui::PopupCloseBehavior,
    EguiContexts,
};

use jarvis_avatar::config::Settings;
use jarvis_avatar::icons;
use jarvis_avatar::pose_library::{AnimationFile, slugify};
use jarvis_avatar::theme;

use crate::plugins::anim_layer_sets::LayerSetsStore;
use crate::plugins::anim_layers::{
    BlendMode, BoneMask, DriverKind, Layer, LayerStack, RestPoseSnapshot,
};
use crate::plugins::pose_driver::{BoneHierarchy, BoneSnapshotHandle, IndexedBones, VRM_BONE_NAMES};
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
    /// Filter layer rows by label / slug / kind (case-insensitive substring).
    pub layer_filter: String,
    /// When true, disabled layers are omitted from the scroll list (toggle with Show disabled).
    pub hide_disabled_layers: bool,
    /// Collapsed group headers in the layer list (`group_key` strings).
    pub collapsed_groups: std::collections::HashSet<String>,
    /// Selected VRM expression preset for "Expression preset…" add choice.
    pub picked_expression: String,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum AddDriverChoice {
    #[default]
    Breathing,
    AutoBlink,
    WeightShift,
    FingerFidget,
    ToeFidget,
    LookAround,
    Sway,
    ArmSway,
    LegShift,
    ClipFromLibrary,
    PoseFromLibrary,
    ExpressionPreset,
}

impl AddDriverChoice {
    fn label(self) -> &'static str {
        match self {
            Self::Breathing => "Breathing",
            Self::AutoBlink => "Auto-Blink",
            Self::WeightShift => "Weight Shift",
            Self::FingerFidget => "Finger Fidget",
            Self::ToeFidget => "Toe Fidget",
            Self::LookAround => "Look Around",
            Self::Sway => "Body Sway",
            Self::ArmSway => "Arm Sway",
            Self::LegShift => "Leg Shift",
            Self::ClipFromLibrary => "Clip from Library…",
            Self::PoseFromLibrary => "Pose from Library…",
            Self::ExpressionPreset => "Expression preset…",
        }
    }
}

const ALL_CHOICES: &[AddDriverChoice] = &[
    AddDriverChoice::Breathing,
    AddDriverChoice::AutoBlink,
    AddDriverChoice::WeightShift,
    AddDriverChoice::FingerFidget,
    AddDriverChoice::ToeFidget,
    AddDriverChoice::LookAround,
    AddDriverChoice::Sway,
    AddDriverChoice::ArmSway,
    AddDriverChoice::LegShift,
    AddDriverChoice::ClipFromLibrary,
    AddDriverChoice::PoseFromLibrary,
    AddDriverChoice::ExpressionPreset,
];

/// Fixed-height footer toolbar inside the Animation Layers panel.
const ADD_BAR_HEIGHT: f32 = 34.0;
const STATUS_STRIP_HEIGHT: f32 = 22.0;
/// Layer name field — fixed so transport / reorder buttons stay on one row.
const LAYER_LABEL_WIDTH: f32 = 120.0;
/// Kind tag column — fits `[expression-hold]` so labels align across rows.
const LAYER_KIND_TAG_WIDTH: f32 = 108.0;
const LAYER_WEIGHT_SLIDER_WIDTH: f32 = 96.0;
/// Right-aligned labels for expanded driver / mask rows.
const LAYER_PARAM_LABEL_WIDTH: f32 = 108.0;
/// Max width for widgets in `param_row` (avoids combobox stretching the panel).
const LAYER_PARAM_WIDGET_MAX: f32 = 240.0;

/// Layer-stack resources for the Animation Layers window.
#[derive(SystemParam)]
pub struct AnimLayersWindowParams<'w> {
    pub handle: Option<Res<'w, crate::plugins::anim_layers::LayerStackHandle>>,
    pub library: Option<Res<'w, PoseLibraryAssets>>,
    pub rest: Option<Res<'w, crate::plugins::anim_layers::RestPoseSnapshot>>,
    pub indexed: Option<Res<'w, IndexedBones>>,
    pub hierarchy: Option<Res<'w, BoneHierarchy>>,
    pub layer_sets: Option<Res<'w, LayerSetsStore>>,
    pub snapshot: Option<Res<'w, BoneSnapshotHandle>>,
    pub glitch: Option<ResMut<'w, crate::plugins::anim_layers::LayerGlitchMonitor>>,
}

pub fn draw_anim_layers_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<super::DebugUiState>,
    params: AnimLayersWindowParams,
) {
    if !settings.ui.show_anim_layers {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else { return };
    let Some(handle) = params.handle.as_ref() else { return };

    // Partial-move the glitch monitor out of `params` so the render closure can
    // borrow it mutably (config edits) independently of the immutable borrows it
    // takes on the other `params` fields.
    let mut glitch = params.glitch;

    let available_bones = available_bone_names(params.indexed.as_deref());

    let dock_side = settings.ui.anim_layers_dock_side.clone();
    let bottom_h = settings.ui.anim_layers_bottom_height.max(160.0);

    // Collected during render and applied after the egui borrow scope
    // closes — the closure captures `state.anim_layers` and `handle`
    // mutably, so we can't also touch `settings` from inside it.
    let mut requested_dock_side: Option<String> = None;
    let mut new_height: Option<f32> = None;
    let mut window_open = settings.ui.show_anim_layers;

    let mut render = |ui: &mut egui::Ui| {
        handle.with_write(|stack| {
            menu_bar_row(
                ui,
                &mut state.anim_layers,
                stack,
                params.rest.as_deref(),
                params.layer_sets.as_deref(),
                params.library.as_deref(),
                glitch.as_deref_mut(),
                &dock_side,
                &mut requested_dock_side,
            );
            master_filter_row(ui, &mut state.anim_layers, stack);
            ui.separator();
            egui::TopBottomPanel::bottom("anim_layers_add_bar")
                .exact_height(ADD_BAR_HEIGHT)
                .show_inside(ui, |ui| {
                    let presets = expression_presets(params.snapshot.as_deref());
                    add_layer_bar(
                        ui,
                        &mut state.anim_layers,
                        stack,
                        params.library.as_deref(),
                        &presets,
                    );
                });
            let store_err = params.layer_sets.as_deref().and_then(|store| {
                store.inner.read().last_error.clone()
            });
            let has_status_strip = state.anim_layers.status.is_some() || store_err.is_some();
            if has_status_strip {
                egui::TopBottomPanel::bottom("anim_layers_status_strip")
                    .exact_height(STATUS_STRIP_HEIGHT)
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(msg) = &state.anim_layers.status {
                                ui.colored_label(theme::success(ui), msg);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if let Some(err) = &store_err {
                                        ui.colored_label(theme::error(ui), err);
                                    }
                                },
                            );
                        });
                    });
            }
            layer_list(
                ui,
                &mut state.anim_layers,
                stack,
                &available_bones,
                params.hierarchy.as_deref(),
                glitch.as_deref(),
            );
        });
    };

    match dock_side.as_str() {
        "bottom" => {
            let resp = egui::TopBottomPanel::bottom("anim_layers_bottom_panel")
                .resizable(true)
                .default_height(bottom_h)
                .min_height(160.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            render(ui);
                        });
                });
            new_height = Some(resp.response.rect.height());
        }
        "left" => {
            egui::SidePanel::left("anim_layers_left_panel")
                .resizable(true)
                .default_width(540.0)
                .min_width(360.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            render(ui);
                        });
                });
        }
        "right" => {
            egui::SidePanel::right("anim_layers_right_panel")
                .resizable(true)
                .default_width(540.0)
                .min_width(360.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            render(ui);
                        });
                });
        }
        _ => {
            egui::Window::new("Animation Layers")
                .default_size([680.0, 520.0])
                .min_width(540.0)
                .resizable(true)
                .open(&mut window_open)
                .show(ctx, |ui| {
                    render(ui);
                });
        }
    }

    if dock_side == "floating" {
        settings.ui.show_anim_layers = window_open;
    }
    if let Some(side) = requested_dock_side {
        settings.ui.anim_layers_dock_side = side;
    }
    if let Some(h) = new_height {
        if (h - settings.ui.anim_layers_bottom_height).abs() > 1.0 {
            settings.ui.anim_layers_bottom_height = h;
        }
    }
}

/// Dropdown for toolbar panels with comboboxes / collapsing headers.
/// Default `menu_button` uses `CloseOnClick` and dismisses on any inner click.
fn toolbar_panel_menu<R>(ui: &mut egui::Ui, title: impl Into<egui::WidgetText>, body: impl FnOnce(&mut egui::Ui) -> R) {
    MenuButton::new(title)
        .config(MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside))
        .ui(ui, body);
}

/// Dock-side picker as a single icon menu button (replaces the old
/// Bottom/Left/Right/Float text-button row).
fn dock_menu_button(ui: &mut egui::Ui, current: &str, requested_dock_side: &mut Option<String>) {
    let icon = match current {
        "left" => icons::DOCK_LEFT,
        "right" => icons::DOCK_RIGHT,
        "bottom" => icons::DOCK_BOTTOM,
        _ => icons::FLOATING,
    };
    ui.menu_button(format!("{} {}", icon, icons::CHEV_OPEN), |ui| {
        let mut pick = |ui: &mut egui::Ui, label: String, target: &str| {
            if ui
                .add_enabled(current != target, egui::Button::new(label))
                .clicked()
            {
                *requested_dock_side = Some(target.to_string());
                ui.close();
            }
        };
        pick(ui, format!("{} Bottom", icons::DOCK_BOTTOM), "bottom");
        pick(ui, format!("{} Left", icons::DOCK_LEFT), "left");
        pick(ui, format!("{} Right", icons::DOCK_RIGHT), "right");
        pick(ui, format!("{} Float", icons::FLOATING), "floating");
    })
    .response
    .on_hover_text("Dock side");
}

/// Compact top menu bar: dock picker, play/pause-all, Sets + Glitch popups,
/// and a right-aligned layer/rest/clock readout.
#[allow(clippy::too_many_arguments)]
fn menu_bar_row(
    ui: &mut egui::Ui,
    ui_state: &mut AnimLayersUiState,
    stack: &mut LayerStack,
    rest: Option<&RestPoseSnapshot>,
    store: Option<&LayerSetsStore>,
    library: Option<&PoseLibraryAssets>,
    glitch: Option<&mut crate::plugins::anim_layers::LayerGlitchMonitor>,
    dock_side: &str,
    requested_dock_side: &mut Option<String>,
) {
    ui.horizontal(|ui| {
        dock_menu_button(ui, dock_side, requested_dock_side);
        ui.separator();
        if ui
            .button(icons::icon(icons::PLAY))
            .on_hover_text("Play all layers")
            .clicked()
        {
            for layer in &mut stack.layers {
                layer.playing = true;
            }
            ui_state.status = Some("all layers playing".into());
        }
        if ui
            .button(icons::icon(icons::PAUSE))
            .on_hover_text("Pause all layers")
            .clicked()
        {
            for layer in &mut stack.layers {
                layer.playing = false;
            }
            ui_state.status = Some("all layers paused".into());
        }
        ui.separator();
        toolbar_panel_menu(ui, format!("Sets {}", icons::CHEV_OPEN), |ui| {
            layer_sets_bar(ui, ui_state, stack, store, library);
        });
        if let Some(mon) = glitch {
            toolbar_panel_menu(ui, format!("{} Glitch", icons::STATUS_WARN), |ui| {
                glitch_controls_bar(ui, mon);
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("t {:.1}s", stack.clock))
                    .monospace()
                    .color(theme::weak_text(ui)),
            );
            ui.separator();
            let bones = rest.map(|r| r.captured).unwrap_or(0);
            ui.label(
                egui::RichText::new(format!("{bones} rest"))
                    .monospace()
                    .color(if bones == 0 {
                        theme::warn(ui)
                    } else {
                        theme::success(ui)
                    }),
            )
            .on_hover_text(
                "Bones with captured rest rotations. Procedural layers need this > 0 \
                 (give it a second after VRM load).",
            );
            ui.separator();
            ui.label(egui::RichText::new(format!("{} layers", stack.layers.len())).monospace());
        });
    });
}

/// Master-enable checkbox on the left with a right-aligned filter / hide / solo
/// cluster on the same row. A contextual filtered-actions row only appears
/// while a non-empty filter matches layers.
fn master_filter_row(ui: &mut egui::Ui, ui_state: &mut AnimLayersUiState, stack: &mut LayerStack) {
    let hide_disabled = ui_state.hide_disabled_layers;
    let filter = ui_state.layer_filter.trim().to_string();
    let visible_ids: HashSet<u64> = stack
        .layers
        .iter()
        .filter(|l| layer_visible_in_list(l, &filter, hide_disabled))
        .map(|l| l.id)
        .collect();

    ui.horizontal(|ui| {
        ui.checkbox(&mut stack.master_enabled, "Master enabled")
            .on_hover_text(
                "When off, the stack is a no-op — the rig is fully driven by manual MCP / \
                 idle VRMA / slider writes. On runs every layer below.",
            );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if stack.solo_mode {
                if ui
                    .button(format!("Unsolo ({})", stack.solo_only_ids.len()))
                    .on_hover_text("resume composing every enabled layer")
                    .clicked()
                {
                    stack.solo_mode = false;
                    stack.solo_only_ids.clear();
                    ui_state.status = Some("solo off — all enabled layers compose again".into());
                }
            } else if ui
                .button(format!("Solo ({})", visible_ids.len()))
                .on_hover_text("compose & advance only the layers currently shown in the list")
                .clicked()
            {
                if visible_ids.is_empty() {
                    ui_state.status = Some("nothing visible to solo".into());
                } else {
                    stack.solo_mode = true;
                    stack.solo_only_ids = visible_ids.clone();
                    ui_state.status =
                        Some(format!("solo: {} layer(s)", stack.solo_only_ids.len()));
                }
            }
            ui.checkbox(&mut ui_state.hide_disabled_layers, "Hide disabled")
                .on_hover_text("omit disabled layers from the list below");
            if ui
                .button(icons::icon(icons::CLOSE))
                .on_hover_text("clear filter")
                .clicked()
            {
                ui_state.layer_filter.clear();
            }
            ui.add(
                egui::TextEdit::singleline(&mut ui_state.layer_filter)
                    .hint_text("filter…")
                    .desired_width(150.0),
            );
        });
    });

    let filter_now = ui_state.layer_filter.trim().to_string();
    if !filter_now.is_empty() {
        let matching_ids: Vec<u64> = stack
            .layers
            .iter()
            .filter(|l| layer_matches_filter(l, &filter_now))
            .map(|l| l.id)
            .collect();
        if !matching_ids.is_empty() {
            ui.horizontal(|ui| {
                ui.small(format!("{} match", matching_ids.len()));
                if ui
                    .small_button("Disable")
                    .on_hover_text("disable every matching layer")
                    .clicked()
                {
                    for layer in &mut stack.layers {
                        if matching_ids.contains(&layer.id) {
                            layer.enabled = false;
                        }
                    }
                    ui_state.status = Some(format!("disabled {} layer(s)", matching_ids.len()));
                }
                if ui
                    .small_button("Enable")
                    .on_hover_text("re-enable every matching layer")
                    .clicked()
                {
                    for layer in &mut stack.layers {
                        if matching_ids.contains(&layer.id) {
                            layer.enabled = true;
                        }
                    }
                    ui_state.status = Some(format!("enabled {} layer(s)", matching_ids.len()));
                }
                if ui
                    .small_button(icons::icon(icons::TRASH))
                    .on_hover_text("permanently remove every matching layer")
                    .clicked()
                {
                    let n = matching_ids.len();
                    for id in matching_ids {
                        stack.remove_layer(id);
                    }
                    ui_state.status = Some(format!("deleted {n} layer(s)"));
                }
            });
        }
    }
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
    ui.vertical(|ui| {
        ui.label("Set").on_hover_text(
            "Pick a saved layer set below",
        );
        if names.is_empty() {
            ui.label(egui::RichText::new("(no saved sets)").weak().italics());
        } else {
            let pick_label = if ui_state.picked_set.is_empty() {
                "(pick a saved set)"
            } else {
                ui_state.picked_set.as_str()
            };
            ui.label(egui::RichText::new(pick_label).strong());
            egui::ScrollArea::vertical()
                .id_salt("anim_layer_set_pick_list")
                .max_height(140.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_min_width(200.0);
                    if ui
                        .selectable_label(ui_state.picked_set.is_empty(), "(none)")
                        .clicked()
                    {
                        ui_state.picked_set.clear();
                    }
                    for n in &names {
                        ui.selectable_value(&mut ui_state.picked_set, n.clone(), n);
                    }
                });
        }
        let has_pick = !ui_state.picked_set.is_empty();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_pick, egui::Button::new("Load"))
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
                .add_enabled(has_pick, egui::Button::new("Delete"))
                .on_hover_text("Remove the selected set (save to persist)")
                .clicked()
            {
                let name = ui_state.picked_set.clone();
                store.delete(&name);
                ui_state.picked_set.clear();
                ui_state.status = Some(format!("deleted '{name}'"));
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Save as:");
            ui.add(
                egui::TextEdit::singleline(&mut ui_state.new_set_name)
                    .hint_text("e.g. idle-relaxed")
                    .desired_width(180.0),
            );
        });
        let can_save = !ui_state.new_set_name.trim().is_empty();
        if ui
            .add_enabled(can_save, egui::Button::new("Save current"))
            .on_hover_text("Snapshot every layer above into this set")
            .clicked()
        {
            let name = ui_state.new_set_name.trim().to_string();
            store.save_current(&name, stack);
            ui_state.picked_set = name.clone();
            ui_state.new_set_name.clear();
            ui_state.status = Some(format!("saved '{name}' (click Persist to disk)"));
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .button("Persist")
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
            if ui
                .button("Reload")
                .on_hover_text("Drop in-memory sets and re-read from disk")
                .clicked()
            {
                store.reload();
                ui_state.status = Some("reloaded from disk".into());
            }
        });
    });
}

fn expression_presets(snapshot: Option<&BoneSnapshotHandle>) -> Vec<String> {
    let mut names: Vec<String> = snapshot
        .map(|s| s.0.read().expression_presets.clone())
        .unwrap_or_default();
    names.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
    names.dedup();
    names
}

/// Humanoid groupings for per-bone clip layers (matches Bones tab order).
const ANIM_LAYER_BONE_GROUPS: &[(&str, &[&str])] = &[
    (
        "Torso",
        &["hips", "spine", "chest", "upperChest", "neck", "head"],
    ),
    ("Face", &["jaw", "leftEye", "rightEye"]),
    (
        "Left Arm",
        &["leftShoulder", "leftUpperArm", "leftLowerArm", "leftHand"],
    ),
    (
        "Right Arm",
        &[
            "rightShoulder",
            "rightUpperArm",
            "rightLowerArm",
            "rightHand",
        ],
    ),
    (
        "Left Leg",
        &["leftUpperLeg", "leftLowerLeg", "leftFoot", "leftToes"],
    ),
    (
        "Right Leg",
        &["rightUpperLeg", "rightLowerLeg", "rightFoot", "rightToes"],
    ),
    (
        "Left Hand Fingers",
        &[
            "leftThumbMetacarpal",
            "leftThumbProximal",
            "leftThumbDistal",
            "leftIndexProximal",
            "leftIndexIntermediate",
            "leftIndexDistal",
            "leftMiddleProximal",
            "leftMiddleIntermediate",
            "leftMiddleDistal",
            "leftRingProximal",
            "leftRingIntermediate",
            "leftRingDistal",
            "leftLittleProximal",
            "leftLittleIntermediate",
            "leftLittleDistal",
        ],
    ),
    (
        "Right Hand Fingers",
        &[
            "rightThumbMetacarpal",
            "rightThumbProximal",
            "rightThumbDistal",
            "rightIndexProximal",
            "rightIndexIntermediate",
            "rightIndexDistal",
            "rightMiddleProximal",
            "rightMiddleIntermediate",
            "rightMiddleDistal",
            "rightRingProximal",
            "rightRingIntermediate",
            "rightRingDistal",
            "rightLittleProximal",
            "rightLittleIntermediate",
            "rightLittleDistal",
        ],
    ),
];

const BONE_SUBGROUP_ORDER: &[&str] = &[
    "Torso",
    "Face",
    "Left Arm",
    "Right Arm",
    "Left Leg",
    "Right Leg",
    "Left Hand Fingers",
    "Right Hand Fingers",
];

/// Controls for the per-layer jitter/glitch detector. Lets the user toggle the
/// detector, tune how aggressively it flags discontinuities, and clear stale
/// flashes. The flash itself is drawn in `layer_row`.
fn glitch_controls_bar(
    ui: &mut egui::Ui,
    mon: &mut crate::plugins::anim_layers::LayerGlitchMonitor,
) {
    ui.vertical(|ui| {
        ui.checkbox(&mut mon.enabled, format!("{} Glitch detect", icons::STATUS_WARN))
            .on_hover_text(
                "flash a layer's row when its bones jump faster than expected \
                 (spots arm/leg/sway jitter as it happens)",
            );
        if !mon.enabled {
            return;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Sensitivity").on_hover_text(
                "lower = flag smaller spikes (multiplier over each layer's moving baseline)",
            );
            ui.add(
                egui::Slider::new(&mut mon.sensitivity, 1.5..=12.0)
                    .fixed_decimals(1)
                    .suffix("×"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Floor").on_hover_text(
                "ignore spikes below this absolute angular speed (deg/s) — kills noise",
            );
            ui.add(
                egui::Slider::new(&mut mon.floor_dps, 10.0..=180.0)
                    .fixed_decimals(0)
                    .suffix("°/s"),
            );
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(format!("{} log", mon.log.len()))
                .on_hover_text("spikes captured since the log was last cleared");
            if ui
                .add_enabled(!mon.log.is_empty(), egui::Button::new("Copy"))
                .on_hover_text("copy the full glitch log to the clipboard")
                .clicked()
            {
                ui.ctx().copy_text(format_glitch_log(mon));
            }
            if ui
                .add_enabled(!mon.log.is_empty(), egui::Button::new("Clear log"))
                .on_hover_text("empty the glitch log")
                .clicked()
            {
                mon.log.clear();
                mon.events.clear();
            }
        });
    });

    if mon.enabled && !mon.log.is_empty() {
        egui::CollapsingHeader::new(format!("glitch log ({})", mon.log.len()))
            .default_open(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        // Newest last so the bottom (stuck) shows the latest pop.
                        for ev in &mon.log {
                            ui.label(
                                egui::RichText::new(format_glitch_line(ev)).monospace(),
                            );
                        }
                    });
            });
    }
}

/// One log line: layer timeline position first (what the user asked for), then
/// the spike magnitude / bone / baseline ratio, then the monitor wall clock.
fn format_glitch_line(ev: &crate::plugins::anim_layers::GlitchEvent) -> String {
    format!(
        "t={:6.2}s  {:<16}  {:>4.0}°/s  {:<16}  {:>4.1}x  @{:.1}s",
        ev.layer_time, ev.layer_label, ev.peak_dps, ev.bone, ev.ratio, ev.at,
    )
}

/// The whole log as copyable text, oldest → newest.
fn format_glitch_log(mon: &crate::plugins::anim_layers::LayerGlitchMonitor) -> String {
    let mut out = String::from(
        "# layer-timeline-t   layer            peak°/s  bone              ratio  monitor-t\n",
    );
    for ev in &mon.log {
        out.push_str(&format_glitch_line(ev));
        out.push('\n');
    }
    out
}

fn clip_label_parent(label: &str) -> String {
    label
        .split('·')
        .next()
        .unwrap_or(label)
        .trim()
        .to_string()
}

fn clip_label_suffix(label: &str) -> Option<&str> {
    label.split('·').nth(1).map(str::trim).filter(|s| !s.is_empty())
}

fn primary_bone_for_layer(layer: &Layer) -> String {
    if let Some(bone) = layer.mask.include.first() {
        return bone.clone();
    }
    clip_label_suffix(&layer.label)
        .map(str::to_string)
        .unwrap_or_else(|| "?".to_string())
}

fn def_bone_category_key(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let rest = lower.strip_prefix("def-")?;
    if rest.is_empty() {
        return None;
    }
    let end = rest
        .find(|c: char| c == '.' || c == '_')
        .unwrap_or(rest.len());
    let cat = rest[..end].trim_matches('-');
    (!cat.is_empty()).then(|| cat.to_string())
}

fn humanoid_bone_group(bone: &str) -> Option<&'static str> {
    ANIM_LAYER_BONE_GROUPS
        .iter()
        .find(|(_, bones)| bones.contains(&bone))
        .map(|(name, _)| *name)
}

fn bone_clip_list_group(bone: &str) -> String {
    if let Some(group) = humanoid_bone_group(bone) {
        return format!("Bone · {group}");
    }
    if let Some(cat) = def_bone_category_key(bone) {
        return format!("Bone · DEF · {cat}");
    }
    let prefix = crate::plugins::spring_preset::bone_name_prefix(bone);
    format!("Bone · {prefix}")
}

fn group_sort_key(group: &str) -> (u8, String) {
    match group {
        "Procedural" => (0, String::new()),
        "Pose holds" => (1, String::new()),
        "Expression presets" => (2, String::new()),
        s if s.starts_with("Bone · DEF · ") => (3, format!("1{}", &s[13..])),
        s if s.starts_with("Bone · ") => {
            let tail = &s[7..];
            let rank = BONE_SUBGROUP_ORDER
                .iter()
                .position(|g| *g == tail)
                .unwrap_or(BONE_SUBGROUP_ORDER.len());
            (3, format!("0{rank:02}{tail}"))
        }
        s if s.starts_with("Morph · ") => (4, s[8..].to_string()),
        s if s.starts_with("Clip · ") => (5, s[7..].to_string()),
        _ => (9, group.to_string()),
    }
}

fn layer_list_group_key(layer: &Layer) -> String {
    match layer.driver.kind_label() {
        "breathing" | "auto-blink" | "weight-shift" | "finger-fidget" | "toe-fidget"
        | "look-around" | "sway" | "arm-sway" | "leg-shift" => "Procedural".to_string(),
        "expression-hold" => "Expression presets".to_string(),
        "pose-hold" => "Pose holds".to_string(),
        "clip" if layer.slug.starts_with("bone-") => {
            bone_clip_list_group(&primary_bone_for_layer(layer))
        }
        "clip" if layer.slug.starts_with("expr-") => {
            format!("Morph · {}", clip_label_parent(&layer.label))
        }
        "clip" if layer.slug == "expressions" || layer.label.contains("expressions") => {
            format!("Morph · {}", clip_label_parent(&layer.label))
        }
        "clip" => format!("Clip · {}", clip_label_parent(&layer.label)),
        _ => "Other".to_string(),
    }
}

fn layer_visible_in_list(layer: &Layer, filter: &str, hide_disabled: bool) -> bool {
    if hide_disabled && !layer.enabled {
        return false;
    }
    layer_matches_filter(layer, filter)
}

fn layer_matches_filter(layer: &Layer, filter: &str) -> bool {
    let f = filter.trim().to_ascii_lowercase();
    if f.is_empty() {
        return true;
    }
    let group = layer_list_group_key(layer);
    layer.label.to_ascii_lowercase().contains(&f)
        || layer.slug.to_ascii_lowercase().contains(&f)
        || layer.driver.kind_label().contains(&f)
        || group.to_ascii_lowercase().contains(&f)
        || layer.mask.include.iter().any(|b| b.to_ascii_lowercase().contains(&f))
}

fn layer_list(
    ui: &mut egui::Ui,
    ui_state: &mut AnimLayersUiState,
    stack: &mut LayerStack,
    bone_names: &[String],
    hierarchy: Option<&BoneHierarchy>,
    glitch: Option<&crate::plugins::anim_layers::LayerGlitchMonitor>,
) {
    let mut to_remove: Option<u64> = None;
    let mut to_move: Option<(usize, isize)> = None;
    let mut to_duplicate: Option<(u64, bool)> = None;

    let filter = ui_state.layer_filter.clone();
    let hide_disabled = ui_state.hide_disabled_layers;
    let mut groups: HashMap<String, Vec<(usize, u64)>> = HashMap::new();
    for (idx, layer) in stack.layers.iter().enumerate() {
        if !layer_visible_in_list(layer, &filter, hide_disabled) {
            continue;
        }
        groups
            .entry(layer_list_group_key(layer))
            .or_default()
            .push((idx, layer.id));
    }
    let mut group_keys: Vec<String> = groups.keys().cloned().collect();
    group_keys.sort_by(|a, b| group_sort_key(a).cmp(&group_sort_key(b)));

    egui::ScrollArea::vertical()
        .id_salt("anim_layers_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width());
            if stack.layers.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    ui.label(egui::RichText::new("No layers yet").italics());
                    ui.small("Use + Add or Install defaults in the toolbar below.");
                });
                return;
            }
            if group_keys.is_empty() {
                if hide_disabled && stack.layers.iter().any(|l| !l.enabled) {
                    ui.label(egui::RichText::new("All visible layers are hidden (disabled)").italics());
                    ui.small("Click Show disabled above to reveal them.");
                } else {
                    ui.label(egui::RichText::new("No layers match filter").italics());
                }
                return;
            }
            for group in group_keys {
                let Some(entries) = groups.get(&group) else {
                    continue;
                };
                let mut open = !ui_state.collapsed_groups.contains(&group);
                ui.horizontal(|ui| {
                    if ui
                        .small_button(icons::icon(if open {
                            icons::CHEV_OPEN
                        } else {
                            icons::CHEV_CLOSED
                        }))
                        .on_hover_text("expand / collapse group")
                        .clicked()
                    {
                        open = !open;
                    }
                    ui.label(
                        egui::RichText::new(format!("{group} ({})", entries.len())).strong(),
                    );
                });
                if open {
                    ui_state.collapsed_groups.remove(&group);
                    ui.indent(format!("grp_{group}"), |ui| {
                        for &(idx, id) in entries {
                            let Some(layer) = stack.layers.iter_mut().find(|l| l.id == id) else {
                                continue;
                            };
                            let action =
                                layer_row(ui, ui_state, idx, layer, bone_names, hierarchy, glitch);
                            match action {
                                Some(LayerAction::Delete) => to_remove = Some(layer.id),
                                Some(LayerAction::MoveUp) => to_move = Some((idx, -1)),
                                Some(LayerAction::MoveDown) => to_move = Some((idx, 1)),
                                Some(LayerAction::Duplicate { id, flip_reverse }) => {
                                    to_duplicate = Some((id, flip_reverse));
                                }
                                None => {}
                            }
                        }
                    });
                } else {
                    ui_state.collapsed_groups.insert(group);
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
    if let Some((id, flip_reverse)) = to_duplicate {
        if let Some(new_id) = stack.duplicate_layer(id, flip_reverse) {
            ui_state.status = Some(if flip_reverse {
                format!("duplicated layer {id} reversed {} id {new_id}", icons::ARROW_RIGHT)
            } else {
                format!("duplicated layer {id} {} id {new_id}", icons::ARROW_RIGHT)
            });
        }
    }
}

enum LayerAction {
    Delete,
    MoveUp,
    MoveDown,
    Duplicate {
        id: u64,
        flip_reverse: bool,
    },
}

fn layer_supports_reverse(layer: &Layer) -> bool {
    matches!(
        layer.driver,
        DriverKind::Clip { .. } | DriverKind::ExpressionHold { .. }
    )
}

fn layer_row(
    ui: &mut egui::Ui,
    ui_state: &mut AnimLayersUiState,
    idx: usize,
    layer: &mut Layer,
    bone_names: &[String],
    hierarchy: Option<&BoneHierarchy>,
    glitch: Option<&crate::plugins::anim_layers::LayerGlitchMonitor>,
) -> Option<LayerAction> {
    let mut action: Option<LayerAction> = None;
    let header_color = kind_color(layer.driver.kind_label());
    let frame_color = if layer.enabled {
        egui::Color32::from_rgba_unmultiplied(
            header_color.r(),
            header_color.g(),
            header_color.b(),
            28,
        )
    } else {
        egui::Color32::from_gray(30)
    };
    let expanded = ui_state.expanded.contains(&layer.id);

    // Glitch flash: if this layer popped within `flash_secs`, ring its frame in
    // a fading amber→red stroke. The spike details go to the copyable log in the
    // controls bar (the flash is too quick to read), so the row itself stays clean.
    let flash_k = glitch.and_then(|mon| {
        let ev = mon.events.get(&layer.id)?;
        let age = mon.now - ev.at;
        (age >= 0.0 && age <= mon.flash_secs).then(|| {
            ui.ctx().request_repaint(); // keep the fade animating
            1.0 - (age / mon.flash_secs).clamp(0.0, 1.0) // 1 fresh → 0 expired
        })
    });

    let mut frame = egui::Frame::group(ui.style()).fill(frame_color);
    if let Some(k) = flash_k {
        let a = (40.0 + 215.0 * k) as u8;
        frame = frame.stroke(egui::Stroke::new(
            1.0 + 1.5 * k,
            egui::Color32::from_rgba_unmultiplied(255, 140, 40, a),
        ));
    }
    frame
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width());
            // Row 1: enable + expand + name on the left, kind tag + transport on the right.
            ui.horizontal(|ui| {
                ui.checkbox(&mut layer.enabled, "");
                if ui
                    .small_button(icons::icon(if expanded {
                        icons::CHEV_OPEN
                    } else {
                        icons::CHEV_CLOSED
                    }))
                    .on_hover_text(if expanded {
                        "collapse layer"
                    } else {
                        "expand layer"
                    })
                    .clicked()
                {
                    if expanded {
                        ui_state.expanded.remove(&layer.id);
                    } else {
                        ui_state.expanded.insert(layer.id);
                    }
                }
                if expanded {
                    ui.add(
                        egui::TextEdit::singleline(&mut layer.label)
                            .desired_width(LAYER_LABEL_WIDTH)
                            .min_size(egui::vec2(LAYER_LABEL_WIDTH, 0.0)),
                    );
                } else {
                    let name_resp = ui
                        .button(egui::RichText::new(layer.label.as_str()).strong())
                        .on_hover_text("expand layer — driver params, mask, blend");
                    if name_resp.clicked() {
                        ui_state.expanded.insert(layer.id);
                    }
                }

                let right_w = ui.available_width().max(0.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(right_w, ui.spacing().interact_size.y),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui
                            .button(icons::icon(icons::TRASH))
                            .on_hover_text("delete layer")
                            .clicked()
                        {
                            action = Some(LayerAction::Delete);
                        }
                        if ui
                            .button(icons::icon(icons::DOWN))
                            .on_hover_text("move down")
                            .clicked()
                        {
                            action = Some(LayerAction::MoveDown);
                        }
                        if ui
                            .button(icons::icon(icons::UP))
                            .on_hover_text("move up")
                            .clicked()
                        {
                            action = Some(LayerAction::MoveUp);
                        }
                        if layer_supports_reverse(layer) {
                            if ui
                                .button("DRev")
                                .on_hover_text(
                                    "duplicate below with reversed playback (toe ripple chains)",
                                )
                                .clicked()
                            {
                                action = Some(LayerAction::Duplicate {
                                    id: layer.id,
                                    flip_reverse: true,
                                });
                            }
                            if ui
                                .button("Dup")
                                .on_hover_text("duplicate layer directly below")
                                .clicked()
                            {
                                action = Some(LayerAction::Duplicate {
                                    id: layer.id,
                                    flip_reverse: false,
                                });
                            }
                            if ui
                                .selectable_label(layer.reverse, icons::icon(icons::REVERSE))
                                .on_hover_text("play this clip backwards")
                                .clicked()
                            {
                                layer.reverse = !layer.reverse;
                            }
                        }
                        if ui
                            .button(icons::icon(icons::REWIND))
                            .on_hover_text("rewind to start")
                            .clicked()
                        {
                            layer.time = 0.0;
                        }
                        let transport = if layer.playing { icons::PAUSE } else { icons::PLAY };
                        if ui
                            .button(icons::icon(transport))
                            .on_hover_text("play / pause")
                            .clicked()
                        {
                            layer.playing = !layer.playing;
                        }
                        ui.separator();
                        ui.add_sized(
                            [LAYER_WEIGHT_SLIDER_WIDTH, ui.spacing().interact_size.y],
                            egui::Slider::new(&mut layer.weight, 0.0..=1.0)
                                .fixed_decimals(2)
                                .show_value(true),
                        );
                        ui.label("wgt");
                        ui.separator();
                        let kind = format!("[{}]", layer.driver.kind_label());
                        let kind_resp = ui.add(
                            egui::Button::new(
                                egui::RichText::new(kind).color(header_color),
                            )
                            .frame(false)
                            .min_size(egui::vec2(LAYER_KIND_TAG_WIDTH, ui.spacing().interact_size.y)),
                        );
                        if kind_resp.clicked() {
                            if expanded {
                                ui_state.expanded.remove(&layer.id);
                            } else {
                                ui_state.expanded.insert(layer.id);
                            }
                        }
                        kind_resp.on_hover_text(if expanded {
                            "collapse layer"
                        } else {
                            "expand layer"
                        });
                    },
                );
            });

            // Row 2: timeline
            timeline(ui, layer);

            // Row 3 (optional): driver params.
            if ui_state.expanded.contains(&layer.id) {
                ui.add_space(4.0);
                ui.separator();
                driver_params(ui, layer);
                ui.separator();
                mask_and_blend(ui, layer, bone_names, hierarchy);
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
        egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 255, 240)),
    );
    // Time text.
    let label = if layer.duration.is_some() {
        let rev = if layer.reverse {
            format!(" {}", icons::REVERSE)
        } else {
            String::new()
        };
        format!("{:0.2} / {:0.2}s{rev}", t, duration)
    } else {
        format!("{}  phase {:0.2}s", icons::INFINITY, t)
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
            param_slider(ui, "speed", &mut layer.speed, 0.0..=2.5);
            param_row(ui, "playback", |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut layer.looping, "loop");
                    ui.checkbox(&mut layer.reverse, "reverse");
                    ui.checkbox(&mut layer.ping_pong, "ping-pong")
                        .on_hover_text("bounce at the ends instead of wrapping (seamless, ignores reverse)");
                });
            });
            param_row(ui, "loop crossfade (s)", |ui| {
                ui.add(egui::Slider::new(&mut layer.loop_fade, 0.0..=1.0).fixed_decimals(2))
                    .on_hover_text("blend the loop's tail into its first frame to kill the restart twitch (0 = hard cut)");
            });
            if let Some(dur) = layer.duration {
                param_row(ui, "phase (s)", |ui| {
                    ui.add(
                        egui::Slider::new(&mut layer.time, 0.0..=dur)
                            .fixed_decimals(2)
                            .show_value(true),
                    )
                    .on_hover_text("stagger duplicates for ripple waves");
                });
            }
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
        DriverKind::ExpressionHold { expressions } => {
            ui.label(format!(
                "expression preset layer · {} weight(s)",
                expressions.len()
            ));
            let mut remove: Option<String> = None;
            for (name, weight) in expressions.iter_mut() {
                param_row(ui, name.as_str(), |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Slider::new(weight, 0.0..=1.0).fixed_decimals(2));
                        if ui.button(icons::icon(icons::CLOSE)).clicked() {
                            remove = Some(name.clone());
                        }
                    });
                });
            }
            if let Some(name) = remove {
                expressions.remove(&name);
            }
            param_row(ui, "duration (s)", |ui| {
                let mut dur = layer.duration.unwrap_or(2.0);
                if ui
                    .add(egui::Slider::new(&mut dur, 0.1..=12.0).fixed_decimals(2))
                    .changed()
                {
                    layer.duration = Some(dur);
                }
            });
            param_slider(ui, "speed", &mut layer.speed, 0.0..=2.5);
            param_row(ui, "playback", |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut layer.looping, "loop");
                    ui.checkbox(&mut layer.reverse, "reverse");
                });
            });
            ui.small(
                "Preset weight follows the layer timeline (ramp or triangle pulse when looping).",
            );
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
            curl_bias_deg,
            curl_bias_thumb_deg,
        } => {
            slider(ui, "amplitude (°)", amplitude_deg, 0.0..=6.0);
            slider(ui, "frequency (Hz)", frequency_hz, 0.05..=1.5);
            slider(ui, "curl bias (°)", curl_bias_deg, -20.0..=30.0);
            slider(ui, "thumb opposition (°)", curl_bias_thumb_deg, -10.0..=30.0);
            param_row(ui, "seed", |ui| {
                ui.horizontal(|ui| {
                    ui.monospace(format!("{:#x}", seed));
                    if ui.button("reshuffle").clicked() {
                        *seed = rand::random::<u64>();
                    }
                });
            });
        }
        DriverKind::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
            curl_bias_deg,
        } => {
            slider(ui, "amplitude (°)", amplitude_deg, 0.0..=6.0);
            slider(ui, "frequency (Hz)", frequency_hz, 0.05..=1.5);
            slider(ui, "curl bias (°)", curl_bias_deg, -20.0..=30.0);
            param_row(ui, "seed", |ui| {
                ui.horizontal(|ui| {
                    ui.monospace(format!("{:#x}", seed));
                    if ui.button("reshuffle").clicked() {
                        *seed = rand::random::<u64>();
                    }
                });
            });
        }
        DriverKind::LookAround {
            mean_interval,
            yaw_deg,
            pitch_deg,
            damp,
            ..
        } => {
            slider(ui, "mean interval (s)", mean_interval, 0.5..=12.0);
            slider(ui, "yaw (°)", yaw_deg, 0.0..=30.0);
            slider(ui, "pitch (°)", pitch_deg, 0.0..=20.0);
            ui.label(
                egui::RichText::new(format!(
                    "gaze damp: {:.2} (1 = free, {}0 = locked forward while a face is tracked)",
                    damp,
                    icons::ARROW_RIGHT
                ))
                .small()
                .color(egui::Color32::from_gray(170)),
            );
        }
        DriverKind::Sway {
            rate_hz,
            amount_deg,
        } => {
            slider(ui, "rate (Hz)", rate_hz, 0.01..=0.3);
            slider(ui, "amount (°)", amount_deg, 0.0..=5.0);
        }
        DriverKind::ArmSway {
            rate_hz,
            amount_deg,
        } => {
            slider(ui, "rate (Hz)", rate_hz, 0.01..=0.4);
            slider(ui, "amount (°)", amount_deg, 0.0..=6.0);
        }
        DriverKind::LegShift {
            rate_hz,
            shift_deg,
            knee_bend_deg,
            hip_sway_deg,
            ankle_deg,
            seed,
        } => {
            slider(ui, "rate (Hz)", rate_hz, 0.01..=0.2);
            slider(ui, "hip shift (°)", shift_deg, 0.0..=8.0);
            slider(ui, "knee bend (°)", knee_bend_deg, 0.0..=20.0);
            slider(ui, "hip sway (°)", hip_sway_deg, 0.0..=6.0);
            slider(ui, "ankle sway (°)", ankle_deg, 0.0..=5.0);
            param_row(ui, "seed", |ui| {
                ui.horizontal(|ui| {
                    ui.monospace(format!("{:#x}", seed));
                    if ui.button("reshuffle").clicked() {
                        *seed = rand::random::<u64>();
                    }
                });
            });
        }
    }
}

fn mask_and_blend(
    ui: &mut egui::Ui,
    layer: &mut Layer,
    bone_names: &[String],
    hierarchy: Option<&BoneHierarchy>,
) {
    param_row(ui, "blend", |ui| {
        egui::ComboBox::from_id_salt(("blend_mode", layer.id))
            .selected_text(layer.blend_mode.label())
            .width(ui.available_width().max(0.0))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut layer.blend_mode, BlendMode::Override, "override");
                ui.selectable_value(
                    &mut layer.blend_mode,
                    BlendMode::RestRelative,
                    "additive (rest-relative)",
                );
            });
    });
    param_slider(ui, "speed", &mut layer.speed, 0.0..=3.0);
    bone_mask_editor(
        ui,
        egui::Id::new(("mask_include", layer.id)),
        "include bones…",
        &mut layer.mask.include,
        &mut layer.mask.include_subtrees,
        bone_names,
        hierarchy,
    );
    bone_mask_editor(
        ui,
        egui::Id::new(("mask_exclude", layer.id)),
        "exclude bones…",
        &mut layer.mask.exclude,
        &mut layer.mask.exclude_subtrees,
        bone_names,
        hierarchy,
    );
    ui.small(format!(
        "Empty include = all bones. Subtree chips ({}) match that bone and all descendants.",
        icons::SUBTREE
    ));
}

/// Chip-style bone mask editor:
/// * chips for each currently-selected bone with an × to remove
/// * subtree-root chips show ↓ and include all indexed descendants at runtime
/// * `+ bone…` combo box listing every unselected bone from the VRM's
///   indexed humanoid set
/// * free-text input for bones that aren't in the humanoid set (rare,
///   but some rigs have extra twist bones etc.)
fn bone_mask_editor(
    ui: &mut egui::Ui,
    id: egui::Id,
    combo_hint: &str,
    selection: &mut Vec<String>,
    subtrees: &mut Vec<String>,
    all_bones: &[String],
    hierarchy: Option<&BoneHierarchy>,
) {
    let subtree_id = id.with("subtree_mode");
    let mut subtree_mode: bool = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(subtree_id).unwrap_or(false));

    let wrap_w = ui.available_width().max(0.0);
    ui.set_max_width(wrap_w);
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(wrap_w);
        let mut flat_remove: Option<usize> = None;
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
                    egui::RichText::new(format!("{bone}  {}", icons::CLOSE))
                        .small()
                        .color(color),
                );
                if ui
                    .add(chip)
                    .on_hover_text(if exists {
                        "exact bone — click to remove"
                    } else {
                        "not present in this VRM — click to remove"
                    })
                    .clicked()
                {
                    flat_remove = Some(i);
                }
            });
        }
        if let Some(i) = flat_remove {
            selection.remove(i);
        }

        let mut subtree_remove: Option<usize> = None;
        for (i, bone) in subtrees.iter().enumerate() {
            ui.scope(|ui| {
                ui.visuals_mut().widgets.inactive.weak_bg_fill =
                    egui::Color32::from_rgb(50, 80, 70);
                let desc = hierarchy
                    .map(|h| h.descendants(bone).len())
                    .unwrap_or(1);
                let chip = egui::Button::new(
                    egui::RichText::new(format!("{bone} {}  {}", icons::SUBTREE, icons::CLOSE))
                        .small()
                        .color(egui::Color32::from_rgb(180, 230, 200)),
                );
                if ui
                    .add(chip)
                    .on_hover_text(format!(
                        "subtree root — {desc} indexed bones — click to remove"
                    ))
                    .clicked()
                {
                    subtree_remove = Some(i);
                }
            });
        }
        if let Some(i) = subtree_remove {
            subtrees.remove(i);
        }

        let blocked: std::collections::HashSet<String> = selection
            .iter()
            .chain(subtrees.iter())
            .cloned()
            .collect();
        let remaining: Vec<&String> = all_bones
            .iter()
            .filter(|b| !blocked.contains(*b))
            .collect();
        egui::ComboBox::from_id_salt(id.with("add_combo"))
            .selected_text(combo_hint)
            .width(170.0)
            .show_ui(ui, |ui| {
                if remaining.is_empty() {
                    ui.label(egui::RichText::new("(all bones selected)").small());
                }
                for bone in remaining {
                    if ui.selectable_label(false, bone).clicked() {
                        if subtree_mode {
                            subtrees.push(bone.clone());
                        } else {
                            selection.push(bone.clone());
                        }
                    }
                }
            });

        ui.checkbox(&mut subtree_mode, format!("{} subtree", icons::SUBTREE))
            .on_hover_text("When checked, bones added via + bone… include that bone and all descendants");

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
                if subtree_mode {
                    if !subtrees.contains(&name) {
                        subtrees.push(name);
                    }
                } else if !selection.contains(&name) {
                    selection.push(name);
                }
            }
            scratch.clear();
        }
        ui.ctx().data_mut(|d| d.insert_temp(scratch_id, scratch));
    });
    ui.ctx().data_mut(|d| d.insert_temp(subtree_id, subtree_mode));
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

fn param_row<R>(ui: &mut egui::Ui, label: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let widget_w = (ui.available_width() - LAYER_PARAM_LABEL_WIDTH - gap)
            .clamp(80.0, LAYER_PARAM_WIDGET_MAX);
        let out = ui
            .allocate_ui_with_layout(
                egui::vec2(widget_w, ui.spacing().interact_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| body(ui),
            )
            .inner;
        ui.allocate_ui_with_layout(
            egui::vec2(LAYER_PARAM_LABEL_WIDTH, ui.spacing().interact_size.y),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.label(label);
            },
        );
        out
    })
    .inner
}

fn param_slider<Num>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
) where
    Num: egui::emath::Numeric,
{
    param_row(ui, label, |ui| {
        ui.add(egui::Slider::new(value, range).fixed_decimals(2));
    });
}

fn slider<Num>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
) where
    Num: egui::emath::Numeric,
{
    param_slider(ui, label, value, range);
}

// ---------------------------------------------------------------------------
// Add-layer bar
// ---------------------------------------------------------------------------

fn add_layer_bar(
    ui: &mut egui::Ui,
    ui_state: &mut AnimLayersUiState,
    stack: &mut LayerStack,
    library: Option<&PoseLibraryAssets>,
    expression_presets: &[String],
) {
    ui.horizontal(|ui| {
        ui.set_height(ADD_BAR_HEIGHT - 6.0);
        egui::ComboBox::from_id_salt("anim_layer_add_kind")
            .selected_text(ui_state.add_kind.label())
            .width(160.0)
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
                    .width(180.0)
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
                    .width(180.0)
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

        if matches!(ui_state.add_kind, AddDriverChoice::ExpressionPreset) {
            egui::ComboBox::from_id_salt("anim_layer_add_expression")
                .selected_text(if ui_state.picked_expression.is_empty() {
                    "(pick preset)"
                } else {
                    ui_state.picked_expression.as_str()
                })
                .width(180.0)
                .show_ui(ui, |ui| {
                    if expression_presets.is_empty() {
                        ui.label("(load a VRM first)");
                    }
                    for preset in expression_presets {
                        ui.selectable_value(
                            &mut ui_state.picked_expression,
                            preset.clone(),
                            preset,
                        );
                    }
                });
        }

        if ui.button(format!("{} Add", icons::PLUS)).clicked() {
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
            .on_hover_text("Add the default procedural stack.")
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
        AddDriverChoice::Breathing => {
            Layer::new("breathing", "Breathing", DriverKind::breathing_default())
                .blend(BlendMode::RestRelative)
                .weight(1.0)
        }
        AddDriverChoice::AutoBlink => {
            Layer::new("auto-blink", "Auto-Blink", DriverKind::blink_default())
                .blend(BlendMode::Override)
                .weight(1.0)
        }
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
        AddDriverChoice::ToeFidget => {
            Layer::new("toe-fidget", "Toe Fidget", DriverKind::toe_fidget_default())
                .blend(BlendMode::RestRelative)
                .weight(0.4)
        }
        AddDriverChoice::LookAround => Layer::new(
            "look-around",
            "Look Around",
            DriverKind::look_around_default(),
        )
        .blend(BlendMode::RestRelative)
        .weight(1.0),
        AddDriverChoice::Sway => Layer::new("sway", "Body Sway", DriverKind::sway_default())
            .blend(BlendMode::RestRelative)
            .weight(0.8),
        AddDriverChoice::ArmSway => {
            Layer::new("arm-sway", "Arm Sway", DriverKind::arm_sway_default())
                .blend(BlendMode::RestRelative)
                .weight(0.6)
        }
        AddDriverChoice::LegShift => {
            Layer::new("leg-shift", "Leg Shift", DriverKind::leg_shift_default())
                .blend(BlendMode::RestRelative)
                .weight(0.85)
        }
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
            Layer::new(
                name.clone(),
                name,
                DriverKind::Clip {
                    animation: Box::new(animation),
                },
            )
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
        AddDriverChoice::ExpressionPreset => {
            if ui_state.picked_expression.is_empty() {
                return Err("pick an expression preset first".into());
            }
            let preset = ui_state.picked_expression.clone();
            let slug = format!("expr-{}", slugify(&preset));
            let mut expressions = HashMap::new();
            expressions.insert(preset.clone(), 1.0);
            let mut layer = Layer::new(
                slug,
                preset.clone(),
                DriverKind::ExpressionHold { expressions },
            )
            .blend(BlendMode::Override)
            .weight(1.0);
            layer.duration = Some(2.0);
            layer.looping = true;
            layer.playing = true;
            layer
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

/// Neon / galactic palette for layer-kind tags and timeline fills.
fn kind_color(kind: &str) -> egui::Color32 {
    match kind {
        "clip" => egui::Color32::from_rgb(0, 229, 255),
        "pose-hold" => egui::Color32::from_rgb(179, 102, 255),
        "expression-hold" => egui::Color32::from_rgb(255, 64, 200),
        "breathing" => egui::Color32::from_rgb(0, 255, 170),
        "auto-blink" => egui::Color32::from_rgb(255, 209, 64),
        "weight-shift" => egui::Color32::from_rgb(150, 80, 255),
        "finger-fidget" => egui::Color32::from_rgb(255, 122, 89),
        "toe-fidget" => egui::Color32::from_rgb(64, 224, 255),
        "look-around" => egui::Color32::from_rgb(148, 255, 70),
        "sway" => egui::Color32::from_rgb(70, 160, 255),
        "arm-sway" => egui::Color32::from_rgb(255, 170, 60),
        "leg-shift" => egui::Color32::from_rgb(124, 110, 255),
        _ => egui::Color32::from_rgb(170, 180, 210),
    }
}
