//! Individual dedicated `egui::Window` systems (everything except Chat).
//!
//! Each `draw_*_window` is an independent Bevy system: it bails out immediately
//! when the matching `settings.ui.show_*` flag is false so closed windows cost
//! almost nothing. The menu bar in [`super::draw_menu_bar`] flips those flags.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_egui::egui;

use jarvis_avatar::act::Emotion;
use jarvis_avatar::avatar_defaults::{avatar_defaults_path, load_avatar_defaults};
use jarvis_avatar::config::Settings;
use jarvis_avatar::icons;
use jarvis_avatar::model_catalog::{list_vrm_models, models_dir, resolve_vrm_load_argument};
use jarvis_avatar::theme;

use super::widgets::{rgb_row, rgba_row, vec3_row};
use super::{AvatarVrmPickerState, DebugUiState};
use crate::plugins::avatar::AvatarDebugStats;
use crate::plugins::avatar_defaults::{
    apply_avatar_defaults_now, save_avatar_defaults_from_snapshot, AvatarDefaultsStatus,
};
use crate::plugins::channel_server::{
    ChatCompleteMessage, HubBroadcast, HubState, LookAtRequestMessage, TtsSpeakMessage,
};
use crate::plugins::pose_driver::{BoneSnapshotHandle, PoseCommand, PoseCommandSender};
use crate::plugins::anim_layer_sets::LayerSetsStore;
use crate::plugins::anim_layers::{
    begin_library_animation_edit, idle_clip_library_filename, LayerStackHandle,
};
use crate::plugins::pose_library_assets::PoseLibraryAssets;
use crate::plugins::vrma_clip_import::{StartVrmaClipImport, VrmaClipImportState};

// ---------- Avatar ------------------------------------------------------------

/// Resources/queries the Avatar panel needs, bundled to stay under Bevy's
/// per-system param limit when combined into the Settings workspace.
#[derive(SystemParam)]
pub struct AvatarPanelParams<'w> {
    pub stats: Res<'w, AvatarDebugStats>,
    pub pose_tx: Option<Res<'w, PoseCommandSender>>,
    pub snapshot: Option<Res<'w, BoneSnapshotHandle>>,
    pub defaults_status: Option<Res<'w, AvatarDefaultsStatus>>,
    pub layer_sets: Option<Res<'w, LayerSetsStore>>,
    pub library: Option<Res<'w, PoseLibraryAssets>>,
    pub stack: Option<Res<'w, LayerStackHandle>>,
    pub import_state: Option<Res<'w, VrmaClipImportState>>,
    pub import_events: MessageWriter<'w, StartVrmaClipImport>,
}

/// Avatar panel — rendered as a tab inside the Settings window.
pub fn avatar_panel(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    state: &mut DebugUiState,
    p: &mut AvatarPanelParams,
) {
    let AvatarPanelParams {
        stats,
        pose_tx,
        snapshot,
        defaults_status,
        layer_sets,
        library,
        stack,
        import_state,
        import_events,
    } = p;
    let mut pending_save_defaults = false;
    let mut pending_apply_defaults = false;
    let mut pending_import_idle = false;
    let mut pending_edit_idle_layers = false;
    {
          egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let a = &mut settings.avatar;
            ui.horizontal(|ui| {
                ui.label("Model");
                ui.monospace(a.model_path.as_str());
            })
            .response
            .on_hover_text(
                "[avatar].model_path. Hot-swap updates this immediately (same queue as MCP \
                 load_vrm). cwd should be the crate root so assets/models resolves on disk.",
            );

            let picker = &mut state.avatar_vrm_picker;
            match list_vrm_models(None) {
                Ok(entries) => {
                    picker.list_error = None;
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("avatar_vrm_pick")
                            .selected_text(
                                picker
                                    .selected_basename
                                    .clone()
                                    .unwrap_or_else(|| "(pick a .vrm)".into()),
                            )
                            .width(230.0)
                            .show_ui(ui, |ui| {
                                for entry in &entries {
                                    ui.selectable_value(
                                        &mut picker.selected_basename,
                                        Some(entry.basename.clone()),
                                        &entry.basename,
                                    );
                                }
                                if entries.is_empty() {
                                    ui.weak("(no .vrm files)");
                                }
                            })
                            .response
                            .on_hover_text(format!("Scanning {}", models_dir().display()));
                        let can_load = picker.selected_basename.is_some() && pose_tx.is_some();
                        if ui
                            .add_enabled(can_load, egui::Button::new("Load"))
                            .on_disabled_hover_text(if pose_tx.is_none() {
                                "PoseCommandSender not available"
                            } else {
                                "Pick a .vrm first"
                            })
                            .clicked()
                        {
                            if let Some(name) = picker.selected_basename.clone() {
                                queue_avatar_vrm_load(pose_tx.as_deref(), name.as_str(), picker);
                            }
                        }
                    });
                }
                Err(e) => {
                    picker.list_error = Some(e);
                }
            }

            if let Some(err) = &picker.list_error {
                ui.colored_label(theme::error(ui), err);
            }
            if let Some(err) = &picker.op_error {
                ui.colored_label(theme::warn(ui), err);
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.add_enabled(
                    false,
                    egui::TextEdit::singleline(&mut a.idle_vrma_path).desired_width(200.0),
                )
                .on_hover_text("Default idle VRMA, spawned with the VRM unless idle via layer stack is on.");
                ui.checkbox(&mut a.idle_use_layer_stack, "idle via layers")
                    .on_hover_text(
                        "Skip VRMA autoplay and drive base idle from avatar_defaults.idle_clip / imported JSON clip layer.",
                    );
            });
            ui.checkbox(
                &mut a.auto_apply_avatar_defaults,
                "auto-apply defaults on load",
            );

            ui.separator();
            let defaults_ui = &mut state.avatar_defaults;
            ui.label("Avatar defaults").on_hover_text(format!(
                "Saves current expression overrides (+ optional rest pose / layer set) to {}",
                avatar_defaults_path(&settings.avatar.model_path).display()
            ));
            ui.horizontal(|ui| {
                ui.label("rest_pose:");
                ui.add(
                    egui::TextEdit::singleline(&mut defaults_ui.rest_pose)
                        .hint_text("optional pose library name")
                        .desired_width(160.0),
                );
                ui.label("layer_set:");
                ui.add(
                    egui::TextEdit::singleline(&mut defaults_ui.layer_set)
                        .hint_text("optional set name")
                        .desired_width(140.0),
                );
            });
            ui.horizontal(|ui| {
                let can_save = snapshot.is_some();
                if ui
                    .add_enabled(can_save, egui::Button::new("Save defaults"))
                    .on_hover_text("Capture current expression overrides")
                    .clicked()
                {
                    pending_save_defaults = true;
                }
                if ui
                    .add_enabled(
                        pose_tx.is_some() && library.is_some() && stack.is_some(),
                        egui::Button::new("Apply now"),
                    )
                    .clicked()
                {
                    pending_apply_defaults = true;
                }
                if ui.button(format!("Import idle {} layers", icons::ARROW_RIGHT)).on_hover_text(
                    "Bake [avatar].idle_vrma_path to JSON @ 10 fps, one layer-stack layer per bone, enable idle via layer stack",
                ).clicked() {
                    pending_import_idle = true;
                }
                if ui
                    .button("Edit idle")
                    .on_hover_text(
                        "Open Animation Layers with one layer per bone. Import idle VRMA first if no JSON clip exists.",
                    )
                    .clicked()
                {
                    pending_edit_idle_layers = true;
                }
            });
            if let Some(msg) = &defaults_ui.message {
                ui.colored_label(theme::success(ui), msg);
            }
            if let Some(st) = defaults_status.as_deref() {
                if let Some(msg) = &st.last_message {
                    ui.small(msg);
                }
                if let Some(err) = &st.last_error {
                    ui.colored_label(theme::error(ui), err);
                }
            }
            if let Some(import) = import_state.as_deref() {
                if let Some(s) = &import.status {
                    ui.small(s);
                }
                if let Some(e) = &import.error {
                    ui.colored_label(theme::error(ui), e);
                }
            }

            let a = &mut settings.avatar;
            ui.separator();
            ui.label("Position").on_hover_text("world_position — pulls the rig toward origin/focus.");
            vec3_row(ui, "pos", &mut a.world_position, -20.0..=20.0);
            ui.add(
                egui::Slider::new(&mut a.uniform_scale, 0.1..=10.0)
                    .logarithmic(true)
                    .text("uniform_scale"),
            );

            ui.separator();
            ui.label("Root-motion locking").on_hover_text("See the Y-axis diagnostics below.");
            ui.checkbox(&mut a.lock_root_xz, "lock_root_xz")
                .on_hover_text("snap hips X/Z to bind pose after VRMA");
            ui.checkbox(&mut a.lock_root_y, "lock_root_y")
                .on_hover_text("snap hips Y to bind pose after VRMA");
            ui.checkbox(&mut a.lock_vrm_root_y, "lock_vrm_root_y")
                .on_hover_text("hard-clamp VRM root entity Y to world_position.y");

            ui.separator();
            y_diagnostics_readout(ui, &stats, a.world_position[1]);

            ui.separator();
            ui.label("Background").on_hover_text("background_color — RGBA linear.");
            rgba_row(ui, &mut a.background_color);

            ui.separator();
            ui.label("Window size").on_hover_text("Restart required to apply.");
            let mut w = a.window_width as i32;
            let mut h = a.window_height as i32;
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut w).range(320..=7680).prefix("w "));
                ui.add(egui::DragValue::new(&mut h).range(240..=4320).prefix("h "));
            });
            a.window_width = w.max(0) as u32;
            a.window_height = h.max(0) as u32;
          });
    }

    if pending_save_defaults {
        let defaults_ui = &mut state.avatar_defaults;
        if let Some(snap) = snapshot.as_ref() {
            let rest = optional_trim(&defaults_ui.rest_pose);
            let set = optional_trim(&defaults_ui.layer_set);
            match save_avatar_defaults_from_snapshot(
                &settings.avatar.model_path,
                &snap.0.read(),
                rest,
                set,
                None,
                settings.avatar.idle_use_layer_stack,
            ) {
                Ok(p) => {
                    defaults_ui.message = Some(format!("saved {} {}", icons::ARROW_RIGHT, p.display()));
                }
                Err(e) => defaults_ui.message = Some(e),
            }
        }
    }
    if pending_apply_defaults {
        let defaults_ui = &mut state.avatar_defaults;
        if let (Some(tx), Some(lib), Some(sets), Some(st)) = (
            pose_tx.as_deref(),
            library.as_deref(),
            layer_sets.as_deref(),
            stack.as_deref(),
        ) {
            if let Some(file) = load_avatar_defaults(&settings.avatar.model_path) {
                match apply_avatar_defaults_now(&settings, &file, tx, lib, sets, st) {
                    Ok(msg) => defaults_ui.message = Some(msg),
                    Err(e) => defaults_ui.message = Some(e),
                }
            } else {
                defaults_ui.message =
                    Some("no avatar_defaults.json for this model".into());
            }
        }
    }
    if pending_import_idle {
        import_events.write(StartVrmaClipImport {
            vrma_path: String::new(),
            output_name: String::new(),
            sample_fps: 10.0,
            add_as_base_layer: true,
            save_to_defaults: true,
            use_layer_stack_for_idle: true,
            per_bone_layers: true,
        });
    }
    if pending_edit_idle_layers {
        settings.ui.show_anim_layers = true;
        settings.avatar.idle_use_layer_stack = true;
        let filename = load_avatar_defaults(&settings.avatar.model_path)
            .and_then(|d| d.idle_clip)
            .unwrap_or_else(|| idle_clip_library_filename(&settings.avatar.model_path));
        let defaults_ui = &mut state.avatar_defaults;
        if let (Some(stack), Some(store), Some(lib)) =
            (stack.as_deref(), layer_sets.as_deref(), library.as_deref())
        {
            match begin_library_animation_edit(&filename, &lib.library, store, stack) {
                Ok(msg) => defaults_ui.message = Some(msg),
                Err(e) => defaults_ui.message = Some(format!(
                    "idle layer edit failed: {e} — run Import idle VRMA {} layers first",
                    icons::ARROW_RIGHT
                )),
            }
        } else {
            defaults_ui.message = Some("layer stack / library unavailable".into());
        }
    }
}

fn optional_trim(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn queue_avatar_vrm_load(
    pose_tx: Option<&PoseCommandSender>,
    load_arg: &str,
    picker: &mut AvatarVrmPickerState,
) {
    picker.op_error = None;
    let Some(tx) = pose_tx else {
        picker.op_error =
            Some("PoseCommandSender unavailable — PoseDriverPlugin must be active.".into());
        return;
    };
    match resolve_vrm_load_argument(load_arg) {
        Ok(asset_path) => {
            tx.send(PoseCommand::LoadVrm { asset_path });
        }
        Err(e) => {
            picker.op_error = Some(e);
        }
    }
}

pub fn draw_avatar_y_diag_inline(
    ui: &mut egui::Ui,
    stats: &AvatarDebugStats,
    target_y: f32,
) {
    y_diagnostics_readout(ui, stats, target_y);
}

fn y_diagnostics_readout(ui: &mut egui::Ui, stats: &AvatarDebugStats, target_y: f32) {
    ui.label("Y-axis diagnostics (this frame):");
    egui::Grid::new("avatar_y_diag_grid")
        .num_columns(2)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            row(ui, "target (world_position.y)", format_y(target_y));
            row(
                ui,
                "VRM root · local Y",
                if stats.has_vrm {
                    format_y(stats.vrm_root_local_y)
                } else {
                    "(no VRM loaded)".into()
                },
            );
            row(
                ui,
                "VRM root · world Y",
                if stats.has_vrm {
                    format_y(stats.vrm_root_world_y)
                } else {
                    "—".into()
                },
            );
            row(
                ui,
                "Hips · local Y",
                if stats.has_hips {
                    format_y(stats.hips_local_y)
                } else {
                    "(hips not resolved)".into()
                },
            );
            row(
                ui,
                "Hips · rest local Y",
                if stats.has_hips {
                    format_y(stats.hips_rest_local_y)
                } else {
                    "—".into()
                },
            );
            row(
                ui,
                "Hips · world Y",
                if stats.has_hips {
                    format_y(stats.hips_world_y)
                } else {
                    "—".into()
                },
            );
        });
    ui.small("drift help").on_hover_text(
        "If 'VRM root · local Y' drifts, `lock_vrm_root_y` will pin it. \
         If 'Hips · local Y' drifts away from 'Hips · rest local Y', `lock_root_y` \
         will pin the hips. If neither drifts but the rig still looks bobbing, it's \
         spine/chest rotation (normal idle breathing).",
    );
}

fn row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(label);
    ui.monospace(value);
    ui.end_row();
}

fn format_y(y: f32) -> String {
    format!("{y:+.5} m")
}

// ---------- Camera ------------------------------------------------------------

/// Camera panel — rendered as a tab inside the Settings window.
pub fn camera_panel(ui: &mut egui::Ui, settings: &mut Settings, state: &mut DebugUiState) {
    {
            let cam = &mut settings.camera;
            ui.label("LMB orbit · MMB pan · scroll zoom");
            ui.separator();

            ui.label("Orbit focus (fallback before VRM snap):");
            vec3_row(ui, "focus", &mut cam.focus, -100.0..=100.0);

            ui.add(egui::Slider::new(&mut cam.initial_radius, 0.1..=50.0).text("initial_radius"));
            ui.add(
                egui::Slider::new(&mut cam.min_radius, 0.001..=5.0)
                    .logarithmic(true)
                    .text("min_radius (zoom in)"),
            );
            ui.add(
                egui::Slider::new(&mut cam.max_radius, 1.0..=500.0).text("max_radius (zoom out)"),
            );

            ui.separator();
            ui.add(
                egui::Slider::new(&mut cam.orbit_sensitivity, 0.05..=5.0).text("orbit_sensitivity"),
            );
            ui.add(egui::Slider::new(&mut cam.pan_sensitivity, 0.05..=5.0).text("pan_sensitivity"));
            ui.add(
                egui::Slider::new(&mut cam.zoom_sensitivity, 0.05..=5.0).text("zoom_sensitivity"),
            );

            ui.separator();
            ui.add(
                egui::Slider::new(&mut cam.orbit_smoothness, 0.0..=0.99).text("orbit_smoothness"),
            );
            ui.add(egui::Slider::new(&mut cam.zoom_smoothness, 0.0..=0.99).text("zoom_smoothness"));
            ui.add(egui::Slider::new(&mut cam.pan_smoothness, 0.0..=0.99).text("pan_smoothness"));

            ui.separator();
            ui.checkbox(&mut cam.focus_follow_vrm, "focus_follow_vrm");
            ui.add(egui::Slider::new(&mut cam.focus_y_lift, -2.0..=3.0).text("focus_y_lift"));
            ui.add(egui::Slider::new(&mut cam.snap_wait_frames, 0..=240).text("snap_wait_frames"));
            ui.checkbox(&mut cam.click_pivot_orbit, "click_pivot_orbit (experimental)")
                .on_hover_text(
                    "When ON, an LMB click over the model sets the orbit pivot to the \
                     nearest bone joint along the click ray (next drag orbits around \
                     that point). PanOrbitCamera always re-aims at `focus`, so the \
                     camera silently re-orients toward the new pivot on click and the \
                     first drag frame can sweep a wide arc — that's why this is \
                     opt-in. A future trackball implementation will fix this.",
                );

            if ui
                .button("Re-center on VRM now")
                .on_hover_text("Snap orbit focus to the current VRM root this frame")
                .clicked()
            {
                state.resnap_requested = true;
            }
    }
}

// ---------- Graphics ----------------------------------------------------------

pub fn draw_basic_graphics_inline(ui: &mut egui::Ui, settings: &mut Settings) {
    let g = &mut settings.graphics;

    let mut samples = g.msaa_samples as i32;
    ui.add(egui::Slider::new(&mut samples, 0..=8).text("msaa_samples"))
        .on_hover_text(
            "0/1 = off; 2/4/8 = multisample. SSAO auto-disables while MSAA >= 2.",
        );
    g.msaa_samples = samples.clamp(0, 8) as u32;
    if g.msaa_samples >= 2 {
        g.advanced.ssao_enabled = false;
    }
    ui.checkbox(&mut g.hdr, "hdr")
        .on_hover_text("Restart required to attach/detach HDR on the camera.");

    ui.separator();
    ui.label("present_mode")
        .on_hover_text("Swapchain policy. Fifo is classic VSync.");
    egui::ComboBox::from_id_salt("graphics_present_mode")
        .selected_text(g.present_mode.clone())
        .show_ui(ui, |ui| {
            for mode in [
                "Fifo",
                "AutoVsync",
                "AutoNoVsync",
                "FifoRelaxed",
                "Mailbox",
                "Immediate",
            ] {
                ui.selectable_value(&mut g.present_mode, mode.to_string(), mode);
            }
        });

    ui.separator();
    ui.add(egui::Slider::new(&mut g.exposure_ev100, -6.0..=17.0).text("exposure_ev100"));

    ui.separator();
    ui.label("Ambient");
    ui.add(
        egui::Slider::new(&mut g.ambient_brightness, 0.0..=5.0).text("ambient_brightness"),
    );
    rgba_row(ui, &mut g.ambient_color);

    ui.separator();
    ui.label("Directional");
    ui.add(
        egui::Slider::new(&mut g.directional_illuminance, 0.0..=200_000.0)
            .logarithmic(true)
            .text("illuminance"),
    );
    ui.checkbox(&mut g.directional_shadows, "shadows_enabled");
    ui.label("position");
    vec3_row(ui, "sun_pos", &mut g.directional_position, -50.0..=50.0);
    ui.label("look_at");
    vec3_row(ui, "sun_look", &mut g.directional_look_at, -50.0..=50.0);

    ui.separator();
    ui.checkbox(&mut g.show_ground_plane, "show_ground_plane");
    ui.add(egui::Slider::new(&mut g.ground_size, 1.0..=400.0).text("ground_size"));
    ui.label("ground_color");
    rgb_row(ui, &mut g.ground_base_color);
}

// ---------- Live / Test -------------------------------------------------------

/// Live / Test bench panel — now rendered as a tab inside the Diagnostics
/// window (see `draw_diagnostics_workspace_window`).
#[allow(clippy::too_many_arguments)]
pub fn live_test_panel(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    test: &mut super::LiveTestUiState,
    hub_state: &HubState,
    hub_out: Option<&HubBroadcast>,
    chat_writer: &mut MessageWriter<ChatCompleteMessage>,
    look_writer: &mut MessageWriter<LookAtRequestMessage>,
    tts_writer: &mut MessageWriter<TtsSpeakMessage>,
) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let (status, color) = if hub_state.peer_count > 0 {
            (
                format!("{} peer(s) connected", hub_state.peer_count),
                theme::success(ui),
            )
        } else if hub_state.bound_to.is_some() {
            ("listening · no peers yet".into(), theme::warn(ui))
        } else {
            ("not bound".into(), theme::error(ui))
        };
        ui.horizontal(|ui| {
            ui.label("Channel hub:");
            ui.colored_label(color, status);
            if let Some(bind) = &hub_state.bound_to {
                ui.label(format!("@ ws://{bind}/ws"));
            }
        });

        ui.separator();
        ui.label("Broadcast input:text to all peers (simulate a Wyoming utterance):");
        ui.horizontal(|ui| {
            let avail = (ui.available_width() - 90.0).max(140.0);
            ui.add_sized([avail, 22.0], egui::TextEdit::singleline(&mut test.input_text));
            let disabled = hub_out.is_none();
            if ui
                .add_enabled(!disabled, egui::Button::new("send"))
                .on_disabled_hover_text("hub broadcaster unavailable")
                .clicked()
            {
                if let Some(out) = hub_out {
                    out.send_input_text(&test.input_text, &settings.ironclaw.module_name);
                }
            }
        });

        ui.separator();
        ui.label("Expression test (fires ACT-style ChatCompleteMessage):");
        ui.horizontal(|ui| {
            egui::ComboBox::from_label("emotion")
                .selected_text(format!("{:?}", test.emotion))
                .show_ui(ui, |ui| {
                    for e in [
                        Emotion::Happy,
                        Emotion::Sad,
                        Emotion::Angry,
                        Emotion::Think,
                        Emotion::Surprised,
                        Emotion::Awkward,
                        Emotion::Question,
                        Emotion::Curious,
                        Emotion::Neutral,
                    ] {
                        ui.selectable_value(&mut test.emotion, e.clone(), format!("{e:?}"));
                    }
                });
            if ui.button("trigger").clicked() {
                let emotion_label = format!("{:?}", test.emotion).to_lowercase();
                chat_writer.write(ChatCompleteMessage {
                    content: format!("<|ACT:{{\"emotion\":\"{emotion_label}\"}}|>test"),
                });
            }
        });

        ui.separator();
        ui.label("Look-at target (rig-local meters):");
        vec3_row(ui, "look", &mut test.look_at, -3.0..=3.0);
        ui.horizontal(|ui| {
            if ui.button("look at point").clicked() {
                look_writer.write(LookAtRequestMessage {
                    local_target: Some(Vec3::from_array(test.look_at)),
                });
            }
            if ui.button("back to cursor").clicked() {
                look_writer.write(LookAtRequestMessage { local_target: None });
            }
        });

        ui.separator();
        ui.label("TTS test (Kokoro):");
        ui.horizontal(|ui| {
            let avail = (ui.available_width() - 90.0).max(140.0);
            ui.add_sized([avail, 22.0], egui::TextEdit::singleline(&mut test.tts_text));
            let disabled = !settings.tts.enabled;
            if ui
                .add_enabled(!disabled, egui::Button::new("speak"))
                .on_disabled_hover_text("tts.enabled is false")
                .clicked()
            {
                tts_writer.write(TtsSpeakMessage {
                    text: test.tts_text.clone(),
                });
            }
        });
    });
}

// ---------- Channel hub (IronClaw protocol) -----------------------------------

pub fn channel_hub_panel(ui: &mut egui::Ui, settings: &mut Settings) {
    let i = &mut settings.ironclaw;
    ui.label("IronClaw WS hub")
        .on_hover_text("We HOST the IronClaw-style WS hub. Peers connect to ws://<this-host>/ws.");
    ui.horizontal(|ui| {
        ui.label("bind_address");
        ui.text_edit_singleline(&mut i.bind_address);
    })
    .response
    .on_hover_text("Restart to rebind.");
    ui.horizontal(|ui| {
        ui.label("auth_token");
        ui.text_edit_singleline(&mut i.auth_token);
    })
    .response
    .on_hover_text("Empty = accept any peer.");
    ui.horizontal(|ui| {
        ui.label("module_name");
        ui.text_edit_singleline(&mut i.module_name);
    })
    .response
    .on_hover_text("Identity on envelopes we publish.");
}

// ---------- Gateway -----------------------------------------------------------

pub fn gateway_panel(ui: &mut egui::Ui, settings: &mut Settings) {
    let g = &mut settings.gateway;
    ui.label("IronClaw gateway")
        .on_hover_text("Used by the chat client; SSE + thread CRUD.");
    ui.horizontal(|ui| {
        ui.label("base_url");
        ui.text_edit_singleline(&mut g.base_url);
    })
    .response
    .on_hover_text("No trailing slash; restart to apply.");
    ui.horizontal(|ui| {
        ui.label("auth_token");
        ui.text_edit_singleline(&mut g.auth_token);
    })
    .response
    .on_hover_text("Overrides IRONCLAW_GATEWAY_TOKEN env; restart to apply.");
    ui.horizontal(|ui| {
        ui.label("default_thread_id");
        ui.text_edit_singleline(&mut g.default_thread_id);
    })
    .response
    .on_hover_text("Empty = use whatever the gateway returns active.");

    let mut t = g.request_timeout_ms as i64;
    if ui
        .add(
            egui::DragValue::new(&mut t)
                .speed(50)
                .range(1_000..=120_000)
                .prefix("timeout_ms "),
        )
        .changed()
    {
        g.request_timeout_ms = t.max(1_000) as u64;
    }
    let mut h = g.history_limit as i64;
    if ui
        .add(
            egui::DragValue::new(&mut h)
                .speed(1)
                .range(1..=500)
                .prefix("history_limit "),
        )
        .changed()
    {
        g.history_limit = h.max(1) as u32;
    }
}

// ---------- TTS ---------------------------------------------------------------

pub fn tts_panel(ui: &mut egui::Ui, settings: &mut Settings) {
    let t = &mut settings.tts;
    ui.checkbox(&mut t.enabled, "enabled");
    ui.label("kokoro_url:");
    ui.text_edit_singleline(&mut t.kokoro_url);
    ui.label("voice:");
    ui.text_edit_singleline(&mut t.voice);
    ui.horizontal(|ui| {
        ui.label("response_format");
        ui.text_edit_singleline(&mut t.response_format);
    })
    .response
    .on_hover_text("wav | pcm | mp3 | …");
    ui.checkbox(&mut t.stream, "stream")
        .on_hover_text("Leave off for one-shot WAV/PCM.");
    let mut sr = t.pcm_sample_rate as i64;
    if ui
        .add(
            egui::DragValue::new(&mut sr)
                .speed(100)
                .range(8000..=48_000)
                .prefix("pcm_sample_rate "),
        )
        .changed()
    {
        t.pcm_sample_rate = sr.clamp(8000, 48_000) as u32;
    }
}

// ---------- Look-at -----------------------------------------------------------

pub fn look_at_panel(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.add(
        egui::Slider::new(&mut settings.look_at.idle_return_speed, 0.0..=20.0)
            .text("idle_return_speed"),
    );
}

// ---------- MCP / Pose / A2F / Kimodo -----------------------------------------

pub fn mcp_panel(ui: &mut egui::Ui, settings: &mut Settings) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label("MCP server").on_hover_text(
            "RMCP streamable-HTTP server exposing the pose / A2F / Kimodo tools to IronClaw \
             (and any other MCP client). Changes marked (restart) take effect on next launch.",
        );
        ui.checkbox(&mut settings.mcp.enabled, "enabled (restart)");
        ui.horizontal(|ui| {
            ui.label("bind_address");
            ui.text_edit_singleline(&mut settings.mcp.bind_address);
        })
        .response
        .on_hover_text("Restart to apply.");
        ui.horizontal(|ui| {
            ui.label("path");
            ui.text_edit_singleline(&mut settings.mcp.path);
        })
        .response
        .on_hover_text("Restart to apply.");
        ui.horizontal(|ui| {
            ui.label("auth_token");
            ui.text_edit_singleline(&mut settings.mcp.auth_token);
        })
        .response
        .on_hover_text("Bearer token; restart to apply, empty = none.");
        ui.colored_label(
            theme::info(ui),
            format!(
                "URL: http://{}{}{}",
                settings.mcp.bind_address,
                if settings.mcp.path.starts_with('/') { "" } else { "/" },
                settings.mcp.path,
            ),
        );

        ui.separator();
        ui.label("Audio2Face-3D").on_hover_text(format!(
            "Tip: run the `a2f_status` MCP tool to probe /v1/health/ready and confirm the \
             gRPC stream opens. Test Kokoro{a}A2F with MCP `a2f_from_text`.",
            a = icons::ARROW_RIGHT
        ));
        ui.checkbox(&mut settings.a2f.enabled, "enabled (restart)");
        ui.checkbox(
            &mut settings.a2f.apply_from_tts,
            format!(
                "apply_from_tts — Kokoro {a} A2F {a} face clip after each chat utterance (restart)",
                a = icons::ARROW_RIGHT
            ),
        );
        ui.horizontal(|ui| {
            ui.label("gRPC endpoint:");
            ui.text_edit_singleline(&mut settings.a2f.endpoint);
        });
        ui.horizontal(|ui| {
            ui.label("health URL:");
            ui.text_edit_singleline(&mut settings.a2f.health_url);
        });
        ui.horizontal(|ui| {
            ui.label("function_id");
            ui.text_edit_singleline(&mut settings.a2f.function_id);
        })
        .response
        .on_hover_text("Match A2F --function-id, e.g. Claire.");

        ui.separator();
        ui.label("Kimodo defaults").on_hover_text(
            "Kimodo connects to our channel hub as a WS peer and consumes `kimodo:generate` \
             envelopes; see kimodo-motion-service.py.",
        );
        let mut dur = settings.kimodo.default_duration_sec;
        if ui
            .add(egui::Slider::new(&mut dur, 0.5..=20.0).text("default_duration_sec"))
            .changed()
        {
            settings.kimodo.default_duration_sec = dur;
        }
        let mut steps = settings.kimodo.default_steps as i32;
        if ui
            .add(egui::Slider::new(&mut steps, 10..=500).text("default_steps"))
            .changed()
        {
            settings.kimodo.default_steps = steps.max(1) as u32;
        }
        let mut to = settings.kimodo.generate_timeout_sec as i64;
        if ui
            .add(egui::Slider::new(&mut to, 10..=600).text("generate_timeout_sec"))
            .changed()
        {
            settings.kimodo.generate_timeout_sec = to.max(1) as u64;
        }
        ui.separator();
        ui.label("Pose / animation library")
            .on_hover_text("Shared with the Node pose-controller.");
        ui.horizontal(|ui| {
            ui.label("poses_dir:");
            ui.text_edit_singleline(&mut settings.pose_library.poses_dir);
        });
        ui.horizontal(|ui| {
            ui.label("animations_dir:");
            ui.text_edit_singleline(&mut settings.pose_library.animations_dir);
        });
        ui.label(
            "Poses are re-read from disk on every MCP tool call, so edits made\n\
             here apply immediately without a reload button.",
        );
    });
}
