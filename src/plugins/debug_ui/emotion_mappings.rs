//! "Emotion Mappings" debug window.
//!
//! Lets the user bind ACT emotion labels (`sensual`, `curious`, …) to a
//! VRM expression preset + an animation from the pose library. Reads and
//! writes the shared [`EmotionMapRes`] and persists to
//! `config/emotions.json` on the "Save" button.
//!
//! Animation dropdowns pull from [`PoseLibraryAssets`] so the list is
//! always in sync with what's on disk. Expression dropdowns are seeded
//! from the canonical VRM preset set — bonus free text input for custom
//! presets baked into the model.
//!
//! The collapsible **Multi-preset expression blend** section edits
//! [`EmotionBinding::expression_blend`] so one ACT emotion can drive several
//! VRM morph weights at once (merged with the primary expression column).

use std::collections::HashSet;

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

use jarvis_avatar::config::Settings;
use jarvis_avatar::emotions::EmotionBinding;

use crate::plugins::emotion_map::EmotionMapRes;
use crate::plugins::pose_library_assets::PoseLibraryAssets;

/// Transient UI scratch (new-row form state).
#[derive(Default)]
pub struct EmotionMappingsUiState {
    pub new_label: String,
    pub status: Option<String>,
    /// Lowercased emotion key for the blend-weights sub-panel.
    pub blend_editor_label: String,
    pub blend_custom_key: String,
}

/// Canonical VRM 1.0 preset names. Custom presets baked into a specific
/// model can still be typed manually via the "custom…" entry.
const VRM_PRESETS: &[&str] = &[
    "neutral",
    "happy",
    "angry",
    "sad",
    "relaxed",
    "surprised",
    "aa",
    "ih",
    "ou",
    "ee",
    "oh",
    "blink",
    "blinkLeft",
    "blinkRight",
    "lookUp",
    "lookDown",
    "lookLeft",
    "lookRight",
    "thinking",
];

pub fn draw_emotion_mappings_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut ui_state: ResMut<super::DebugUiState>,
    map: Option<ResMut<EmotionMapRes>>,
    library: Option<Res<PoseLibraryAssets>>,
) {
    if !settings.ui.show_emotion_mappings {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let Some(mut map) = map else { return };

    let animations = library
        .as_deref()
        .map(|l| l.animations())
        .unwrap_or_default();

    let mut open = settings.ui.show_emotion_mappings;
    egui::Window::new("Emotion Mappings")
        .default_size([720.0, 600.0])
        .resizable(true)
        .open(&mut open)
        .show(ctx, |ui| {
            toolbar(ui, &mut ui_state.emotion_mappings, &mut map);
            ui.separator();
            mappings_table(ui, &mut map, &animations);
            ui.separator();
            expression_blend_panel(ui, &mut ui_state.emotion_mappings, &mut map);
            ui.separator();
            add_row(ui, &mut ui_state.emotion_mappings, &mut map);
            if let Some(status) = &ui_state.emotion_mappings.status {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(170, 200, 170), status);
            }
            if let Some(status) = &map.last_status {
                ui.colored_label(egui::Color32::from_rgb(170, 180, 220), status);
            }
            if let Some(err) = &map.inner.last_error {
                ui.colored_label(egui::Color32::from_rgb(220, 120, 120), err);
            }
        });
    settings.ui.show_emotion_mappings = open;
}

fn toolbar(ui: &mut egui::Ui, _state: &mut EmotionMappingsUiState, map: &mut EmotionMapRes) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{} mappings", map.inner.mappings.len())).monospace());
        ui.separator();
        if ui
            .button("💾 Save")
            .on_hover_text(format!("write → {}", map.inner.path.display()))
            .clicked()
        {
            map.save();
        }
        if ui
            .button("⟳ Reload")
            .on_hover_text("discard unsaved edits")
            .clicked()
        {
            map.reload();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.small(format!("path: {}", map.inner.path.display()));
        });
    });
}

fn expression_blend_panel(
    ui: &mut egui::Ui,
    state: &mut EmotionMappingsUiState,
    map: &mut EmotionMapRes,
) {
    let labels = map.inner.sorted_labels();
    ui.collapsing("Multi-preset expression blend", |ui| {
        ui.label(
            egui::RichText::new(
                "Mix VRM presets per emotion. Values combine with the Expression column — \
                 the primary column wins if it uses the same preset name.",
            )
            .small(),
        );
        if labels.is_empty() {
            ui.label(egui::RichText::new("Add an emotion mapping first.").italics());
            return;
        }

        ui.horizontal(|ui| {
            ui.label("Emotion:");
            egui::ComboBox::from_id_salt("expr_blend_emotion_pick")
                .width(240.0)
                .selected_text(if state.blend_editor_label.is_empty() {
                    "(select emotion)".into()
                } else {
                    state.blend_editor_label.clone()
                })
                .show_ui(ui, |ui| {
                    for l in &labels {
                        if ui
                            .selectable_label(state.blend_editor_label == *l, l.as_str())
                            .clicked()
                        {
                            state.blend_editor_label = l.clone();
                        }
                    }
                });
        });

        let key = state.blend_editor_label.trim().to_ascii_lowercase();
        if key.is_empty() {
            ui.label("Pick an emotion to edit blend weights.");
            return;
        }
        let Some(binding) = map.inner.mappings.get_mut(&key) else {
            ui.colored_label(
                egui::Color32::from_rgb(220, 160, 120),
                "Selected label not found — pick again.",
            );
            return;
        };

        if ui.button("Clear blend weights").clicked() {
            binding.expression_blend.clear();
        }

        ui.add_space(6.0);
        let preset_set: HashSet<&str> = VRM_PRESETS.iter().copied().collect();
        egui::Grid::new("expr_blend_preset_grid")
            .num_columns(2)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                for preset in VRM_PRESETS {
                    let mut v = binding
                        .expression_blend
                        .get(*preset)
                        .copied()
                        .unwrap_or(0.0);
                    ui.label(*preset);
                    let changed = ui
                        .add(
                            egui::Slider::new(&mut v, 0.0..=1.0)
                                .fixed_decimals(2)
                                .show_value(true),
                        )
                        .changed();
                    if changed {
                        if v < 0.001 {
                            binding.expression_blend.remove(*preset);
                        } else {
                            binding.expression_blend.insert((*preset).to_string(), v);
                        }
                    }
                    ui.end_row();
                }
            });

        ui.add_space(8.0);
        ui.label(egui::RichText::new("Custom preset (model-specific)").small());
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.blend_custom_key)
                    .hint_text("e.g. mouthSmile")
                    .desired_width(180.0),
            );
            if ui.button("Add at 0.35").clicked() {
                let s = state.blend_custom_key.trim().to_string();
                if !s.is_empty() {
                    binding.expression_blend.entry(s).or_insert(0.35);
                    state.blend_custom_key.clear();
                }
            }
        });

        let extras: Vec<String> = binding
            .expression_blend
            .keys()
            .filter(|k| !preset_set.contains(k.as_str()))
            .cloned()
            .collect();

        for extra in extras {
            let mut remove = false;
            ui.horizontal(|ui| {
                ui.label(&extra);
                if let Some(w) = binding.expression_blend.get_mut(&extra) {
                    ui.add(
                        egui::Slider::new(w, 0.0..=1.0)
                            .fixed_decimals(2)
                            .show_value(true),
                    );
                }
                if ui.small_button("remove").clicked() {
                    remove = true;
                }
            });
            if remove {
                binding.expression_blend.remove(&extra);
            }
        }
    });
}

fn mappings_table(
    ui: &mut egui::Ui,
    map: &mut EmotionMapRes,
    animations: &[jarvis_avatar::pose_library::AnimationMeta],
) {
    egui::ScrollArea::vertical()
        .id_salt("emotion_mappings_scroll")
        .auto_shrink([false, false])
        .max_height(380.0)
        .show(ui, |ui| {
            let labels = map.inner.sorted_labels();
            if labels.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(20.0);
                    ui.label(egui::RichText::new("No emotion mappings yet.").italics());
                    ui.small("Add one below to bind an ACT emotion to an expression / animation.");
                });
                return;
            }
            egui::Grid::new("emotion_grid")
                .num_columns(6)
                .spacing([10.0, 6.0])
                .striped(true)
                .min_col_width(60.0)
                .show(ui, |ui| {
                    ui.strong("emotion");
                    ui.strong("expression");
                    ui.strong("weight");
                    ui.strong("animation");
                    ui.strong("hold (s)");
                    ui.strong("");
                    ui.end_row();

                    let mut to_remove: Option<String> = None;
                    for label in labels {
                        let Some(binding) = map.inner.mappings.get_mut(&label) else {
                            continue;
                        };
                        ui.monospace(&label);
                        expression_combo(ui, &label, binding);
                        ui.add(
                            egui::Slider::new(&mut binding.expression_weight, 0.0..=1.0)
                                .fixed_decimals(2)
                                .show_value(true),
                        );
                        animation_combo(ui, &label, binding, animations);
                        ui.add(
                            egui::DragValue::new(&mut binding.hold_seconds)
                                .range(0.0..=30.0)
                                .speed(0.1)
                                .fixed_decimals(2)
                                .suffix(" s"),
                        );
                        if ui.button("✖️").on_hover_text("delete").clicked() {
                            to_remove = Some(label);
                        }
                        ui.end_row();
                    }
                    if let Some(label) = to_remove {
                        map.inner.remove(&label);
                    }
                });
        });
}

fn expression_combo(ui: &mut egui::Ui, label: &str, binding: &mut EmotionBinding) {
    let mut enabled = binding.expression.is_some();
    ui.horizontal(|ui| {
        if ui.checkbox(&mut enabled, "").changed() {
            binding.expression = if enabled {
                Some("neutral".into())
            } else {
                None
            };
        }
        let mut current = binding.expression.clone().unwrap_or_default();
        let id = egui::Id::new(("expr_combo", label));
        egui::ComboBox::from_id_salt(id)
            .selected_text(if current.is_empty() {
                "(none)"
            } else {
                current.as_str()
            })
            .show_ui(ui, |ui| {
                for preset in VRM_PRESETS {
                    if ui.selectable_label(current == *preset, *preset).clicked() {
                        current = (*preset).to_string();
                    }
                }
            });
        if !enabled {
            binding.expression = None;
        } else {
            ui.add(
                egui::TextEdit::singleline(&mut current)
                    .desired_width(70.0)
                    .hint_text("custom…"),
            );
            if current.trim().is_empty() {
                binding.expression = None;
            } else {
                binding.expression = Some(current);
            }
        }
    });
}

fn animation_combo(
    ui: &mut egui::Ui,
    label: &str,
    binding: &mut EmotionBinding,
    animations: &[jarvis_avatar::pose_library::AnimationMeta],
) {
    ui.horizontal(|ui| {
        let selected = binding.animation.clone().unwrap_or_default();
        let display_label = if selected.is_empty() {
            "(none)".to_string()
        } else {
            selected.clone()
        };
        let id = egui::Id::new(("anim_combo", label));
        egui::ComboBox::from_id_salt(id)
            .selected_text(display_label)
            .width(180.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(selected.is_empty(), "(none)").clicked() {
                    binding.animation = None;
                }
                for meta in animations {
                    let is_current = selected == meta.filename;
                    let pretty = format!("{} · {}", meta.name, meta.filename);
                    if ui.selectable_label(is_current, pretty).clicked() {
                        binding.animation = Some(meta.filename.clone());
                    }
                }
            });
        let mut looping = binding.looping.unwrap_or(false);
        if ui
            .checkbox(&mut looping, "loop")
            .on_hover_text("override the animation's own `looping` metadata")
            .clicked()
        {
            binding.looping = Some(looping);
        }
    });
}

fn add_row(ui: &mut egui::Ui, state: &mut EmotionMappingsUiState, map: &mut EmotionMapRes) {
    ui.horizontal(|ui| {
        ui.label("add emotion:");
        ui.add(
            egui::TextEdit::singleline(&mut state.new_label)
                .hint_text("e.g. sensual, curious, contemplative")
                .desired_width(220.0),
        );
        let can_add = !state.new_label.trim().is_empty()
            && !map
                .inner
                .mappings
                .contains_key(&state.new_label.trim().to_ascii_lowercase());
        if ui
            .add_enabled(can_add, egui::Button::new("➕ Add"))
            .clicked()
        {
            let label = state.new_label.trim().to_string();
            map.inner.insert(
                &label,
                EmotionBinding {
                    expression: Some("neutral".into()),
                    expression_weight: 1.0,
                    hold_seconds: 2.5,
                    ..Default::default()
                },
            );
            state.status = Some(format!("added '{label}' (don't forget Save)"));
            state.new_label.clear();
        }
    });
    ui.small(
        "Bindings apply when an ACT token (e.g. `[ACT emotion=\"curious\"]`) \
         matches an emotion label here. Emotion lookups are case-insensitive.",
    );
}
