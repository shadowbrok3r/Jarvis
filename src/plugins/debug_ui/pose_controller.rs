//! Pose Controller window: manual replacement for the Airi `PoseController`
//! Vue widget.
//!
//! Tabs:
//!   * **Actions** — quick apply for poses, start / stop for native animations,
//!     snapshot current rig as a new pose.
//!   * **Library** — list of poses with filter + category swap + rename + delete.
//!   * **Animations** — list of saved animations with per-row play (native vs.
//!     Kimodo-peer) / loop toggle / hold-duration editor / rename / delete.
//!   * **AI Gen** — prompt → Kimodo generate, streaming into the native player.
//!   * **Idle** — random-pick idle loop driven by `Settings::pose_controller`.
//!   * **Expressions** — one slider per VRM expression preset on the loaded
//!     model (from `BoneSnapshot::expression_presets`); drives
//!     [`PoseCommand::SetExpression`].
//!
//! Everything reads / writes [`PoseLibraryAssets`] (the cached wrapper around
//! [`jarvis_avatar::pose_library::PoseLibrary`]); disk mutations bubble the
//! refresh cache automatically.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use bevy::animation::RepeatAnimation;
use bevy::prelude::*;
use bevy_egui::egui::{Layout, TopBottomPanel};
use bevy_egui::{EguiContexts, egui};
use bevy_vrm1::prelude::*;

use jarvis_avatar::config::Settings;
use jarvis_avatar::pose_library::{AnimationMeta, PoseFile};

use crate::kimodo::{GenerateRequest, KimodoClient};
use crate::plugins::native_anim_player::{ActiveNativeAnimation, StreamingAnimation};
use crate::plugins::pose_driver::{
    IndexedBones, PoseCommand, PoseCommandSender, VRM_BONE_NAMES, def_toe_big_yaw_slider_extra_deg,
    is_vrm_humanoid_bone,
};
use crate::plugins::pose_library_assets::PoseLibraryAssets;
use crate::plugins::shared_runtime::SharedTokio;

/// Visual groupings for the manual Bones tab. Order matters — the UI renders
/// each group as a `CollapsingHeader` in this order.
const BONE_GROUPS: &[(&str, &[&str])] = &[
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

fn bone_name_matches_search(filter_lower: &str, bone: &str) -> bool {
    if filter_lower.is_empty() {
        return true;
    }
    bone.to_ascii_lowercase().contains(filter_lower)
}

/// Group key for names like `DEF-toe_littleL` / `DEF-foot.L` / `DEF-upper_arm.R`:
/// prefix `DEF-` (ASCII case-insensitive), then the category run up to the first `.` or `_`.
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

fn is_def_toe_bone(bone: Option<&str>) -> bool {
    bone.is_some_and(|b| b.to_ascii_lowercase().contains("def-toe"))
}

/// Intrinsic XYZ Euler (degrees) for Bones-tab sliders when seeding from a snapshot quaternion.
/// Normalized pose space is identity at bind; `Quat::to_euler` can return equivalent aliases
/// such as (180°, ε, -180°) for near-identity rotations — the next `from_euler` then diverges
/// from the true pose. Snap near-identity to zeros; see pose_driver normalized pose space.
///
/// `bone`: when `Some` and the name matches `DEF-toe*`, use a **wider** geodesic snap (see
/// `DEF_TOE_ALIAS_MAX_ANGLE_DEG`) so tiny skin twists that Euler expands to ±180° on X/Z still
/// read as ~0° in the UI after a good export.
const DEF_TOE_ALIAS_MAX_ANGLE_DEG: f32 = 34.0;
const DEFAULT_ALIAS_MAX_ANGLE_DEG: f32 = 12.0;

fn euler_xyz_deg_intrinsic_stable_for_ui(q: Quat, bone: Option<&str>) -> [f32; 3] {
    let q = q.normalize();
    let q = if q.w < 0.0 { -q } else { q };
    // Tight hemisphere check (legacy).
    if q.w >= 0.999_984_76 {
        return [0.0, 0.0, 0.0];
    }
    // Geodesic angle from identity in degrees — catches Euler aliases where w is slightly
    // below the threshold but the rotation is still only a few degrees (common on toes).
    let v_len = (q.x * q.x + q.y * q.y + q.z * q.z).sqrt();
    let angle_deg = 2.0 * v_len.atan2(q.w.abs()).to_degrees();
    let max_angle = if is_def_toe_bone(bone) {
        DEF_TOE_ALIAS_MAX_ANGLE_DEG
    } else {
        DEFAULT_ALIAS_MAX_ANGLE_DEG
    };
    if angle_deg < max_angle {
        return [0.0, 0.0, 0.0];
    }
    let (ex, ey, ez) = q.to_euler(EulerRot::XYZ);
    [ex.to_degrees(), ey.to_degrees(), ez.to_degrees()]
}

fn wrap_deg_180_signed(d: f32) -> f32 {
    let mut x = d.rem_euclid(360.0);
    if x > 180.0 {
        x -= 360.0;
    }
    if x <= -180.0 {
        x += 360.0;
    }
    x
}

/// Same normalized `pose_q` + `ApplyBones` path the Bones-tab sliders use.
fn send_apply_bones_euler_deg(sender: &PoseCommandSender, bone: &str, deg: [f32; 3]) {
    let yaw_extra = def_toe_big_yaw_slider_extra_deg(bone);
    let q = Quat::from_euler(
        EulerRot::XYZ,
        deg[0].to_radians(),
        (deg[1] + yaw_extra).to_radians(),
        deg[2].to_radians(),
    );
    let mut bones = HashMap::new();
    bones.insert(bone.to_string(), [q.x, q.y, q.z, q.w]);
    sender.send(PoseCommand::ApplyBones {
        bones,
        preserve_omitted_bones: true,
        blend_weight: Some(1.0),
        transition_seconds: Some(0.0),
    });
}

fn format_def_category_title(key: &str) -> String {
    key.split(|c: char| c == '-' || c == '_')
        .filter(|s| !s.is_empty())
        .map(|word| {
            let mut it = word.chars();
            match it.next() {
                None => String::new(),
                Some(first) => {
                    let mut s = first.to_uppercase().to_string();
                    s.extend(it);
                    s
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Per-window transient state (text filter, selected tab, AI-gen draft, …).
pub struct PoseControllerUiState {
    pub tab: PoseControllerTab,
    pub search: String,
    pub category_filter: String,
    pub status: Option<String>,
    pub rename_buf: HashMap<String, String>,
    pub category_buf: HashMap<String, String>,
    pub anim_rename_buf: HashMap<String, String>,
    pub anim_category_buf: HashMap<String, String>,
    pub anim_hold_buf: HashMap<String, f32>,
    pub gen_prompt: String,
    pub gen_duration: f32,
    pub gen_steps: u32,
    pub gen_save_name: String,
    pub gen_stream: bool,
    pub snapshot_name: String,
    pub snapshot_category: String,
    pub default_playback_mode: PlaybackMode,
    /// Per-bone Euler angles (degrees, intrinsic XYZ) driven by the Bones
    /// diagnostic tab. Each drag fires a single-bone `PoseCommand::ApplyBones`
    /// so we can confirm bone writes reach the visible rig.
    pub bone_euler: HashMap<String, [f32; 3]>,
    /// Filter string for the **Bones** tab only (library/animations use [`Self::search`]).
    pub bone_search: String,
    /// Last `expression_presets` list from the live VRM; when it changes, [`Self::expression_sliders`]
    /// is rebuilt (weights preserved for names that still exist).
    pub expr_tracked_presets: Vec<String>,
    /// 0..=1 weights for the **Expressions** tab; keys are VRMC_vrm preset names.
    pub expression_sliders: HashMap<String, f32>,
}

impl Default for PoseControllerUiState {
    fn default() -> Self {
        Self {
            tab: PoseControllerTab::Actions,
            search: String::new(),
            category_filter: String::new(),
            status: None,
            rename_buf: HashMap::new(),
            category_buf: HashMap::new(),
            anim_rename_buf: HashMap::new(),
            anim_category_buf: HashMap::new(),
            anim_hold_buf: HashMap::new(),
            gen_prompt: "waving energetically with both arms".into(),
            gen_duration: 3.0,
            gen_steps: 100,
            gen_save_name: String::new(),
            gen_stream: true,
            snapshot_name: "my_pose".into(),
            snapshot_category: "custom".into(),
            default_playback_mode: PlaybackMode::Native,
            bone_euler: HashMap::new(),
            bone_search: String::new(),
            expr_tracked_presets: Vec::new(),
            expression_sliders: HashMap::new(),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum PoseControllerTab {
    Actions,
    Library,
    Animations,
    AiGen,
    Idle,
    Expressions,
    Bones,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum PlaybackMode {
    /// Frame-driven by the native Bevy player (reads `AnimationFile.frames`).
    Native,
    /// Forwards `kimodo:play-animation` so the Python peer streams poses back.
    Kimodo,
}

pub fn draw_pose_controller_window(
    mut commands: Commands,
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    library: Option<Res<PoseLibraryAssets>>,
    sender: Option<Res<PoseCommandSender>>,
    mut active_anim: ResMut<ActiveNativeAnimation>,
    streaming: Res<StreamingAnimation>,
    kimodo_client: Option<Res<KimodoClientRes>>,
    tokio_rt: Option<Res<SharedTokio>>,
    snapshot: Option<Res<crate::plugins::pose_driver::BoneSnapshotHandle>>,
    indexed: Option<Res<IndexedBones>>,
    vrma_q: Query<Entity, With<Vrma>>,
    mut players_q: Query<&mut AnimationPlayer>,
    mut state: ResMut<super::DebugUiState>,
) {
    if !settings.ui.show_pose_controller {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let Some(library) = library else {
        return;
    };

    // Keep the window from expanding past the viewport when the Animations /
    // Poses list grows; the inner ScrollArea takes the remaining space.
    let content = ctx.content_rect();
    let max_h = (content.height() - 60.0).max(240.0);
    let max_w = (content.width() - 40.0).max(320.0);

    let vrma_entities: Vec<Entity> = vrma_q.iter().collect();

    let mut open = settings.ui.show_pose_controller;
    egui::Window::new("Pose Controller")
        .default_size([520.0, 520.0])
        .max_height(max_h)
        .max_width(max_w)
        .open(&mut open)
        .show(ctx, |ui| {
            let pc = &mut state.pose_controller;
            tab_bar(ui, pc);
            ui.separator();
            match pc.tab {
                PoseControllerTab::Actions => actions_tab(
                    ui,
                    pc,
                    &library,
                    sender.as_deref(),
                    &mut active_anim,
                    snapshot.as_deref(),
                    &mut commands,
                    &vrma_entities,
                    &mut players_q,
                    &mut settings.pose_controller,
                ),
                PoseControllerTab::Library => library_tab(ui, pc, &library, sender.as_deref()),
                PoseControllerTab::Animations => {
                    animations_tab(ui, pc, &library, &mut active_anim, kimodo_client.as_deref())
                }
                PoseControllerTab::AiGen => ai_gen_tab(
                    ui,
                    pc,
                    &library,
                    &streaming,
                    kimodo_client.as_deref(),
                    tokio_rt.as_deref(),
                ),
                PoseControllerTab::Idle => idle_tab(ui, pc, &mut settings.pose_controller),
                PoseControllerTab::Expressions => expressions_tab(
                    ui,
                    pc,
                    sender.as_deref(),
                    snapshot.as_deref(),
                    &settings.pose_controller,
                ),
                PoseControllerTab::Bones => bones_tab(
                    ui,
                    pc,
                    sender.as_deref(),
                    snapshot.as_deref(),
                    indexed.as_deref(),
                ),
            }

            TopBottomPanel::bottom("Pose Controller Bottom Panel").show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(msg) = &pc.status {
                        ui.label(msg);
                        ui.separator();
                    }

                    ui.with_layout(Layout::right_to_left(egui::Align::Max), |ui| {
                        if let Some(err) = library.last_error() {
                            ui.colored_label(egui::Color32::from_rgb(200, 120, 120), err);
                        }
                    });
                });
            });
        });
    settings.ui.show_pose_controller = open;
}

/// Bevy-side resource wrapping [`KimodoClient`] so the UI can send generate
/// requests. Inserted by the binary's `main.rs` once the hub is up.
#[derive(Resource, Clone)]
pub struct KimodoClientRes(pub KimodoClient);

impl std::ops::Deref for KimodoClientRes {
    type Target = KimodoClient;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// ---------- tab bar ------------------------------------------------------------

fn tab_bar(ui: &mut egui::Ui, state: &mut PoseControllerUiState) {
    ui.horizontal(|ui| {
        tab_btn(ui, &mut state.tab, PoseControllerTab::Actions, "Actions");
        ui.separator();
        tab_btn(ui, &mut state.tab, PoseControllerTab::Library, "Poses");
        ui.separator();
        tab_btn(
            ui,
            &mut state.tab,
            PoseControllerTab::Animations,
            "Animations",
        );
        ui.separator();
        tab_btn(ui, &mut state.tab, PoseControllerTab::AiGen, "AI Gen");
        ui.separator();
        tab_btn(ui, &mut state.tab, PoseControllerTab::Idle, "Idle");
        ui.separator();
        tab_btn(
            ui,
            &mut state.tab,
            PoseControllerTab::Expressions,
            "Expressions",
        );
        ui.separator();
        tab_btn(ui, &mut state.tab, PoseControllerTab::Bones, "Bones");
    });
}

fn tab_btn(
    ui: &mut egui::Ui,
    current: &mut PoseControllerTab,
    value: PoseControllerTab,
    label: &str,
) {
    if ui.selectable_label(*current == value, label).clicked() {
        *current = value;
    }
}

// ---------- Actions tab --------------------------------------------------------

fn actions_tab(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    sender: Option<&PoseCommandSender>,
    active_anim: &mut ResMut<ActiveNativeAnimation>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    commands: &mut Commands,
    vrma_entities: &[Entity],
    players_q: &mut Query<&mut AnimationPlayer>,
    pose_settings: &mut jarvis_avatar::config::PoseControllerSettings,
) {
    let _ = library;
    ui.label("Quick actions:");
    ui.horizontal(|ui| {
        let reset_clicked = ui
            .button("Reset pose + expressions")
            .on_hover_text("Equivalent to the reset_pose MCP tool")
            .clicked();
        if reset_clicked {
            if let Some(s) = sender {
                s.send(PoseCommand::ResetPose);
                state.status = Some("reset pose queued".into());
            }
        }
        if ui.button("Stop native animation").clicked() {
            active_anim.stop();
            state.status = Some("stopped native animation".into());
        }
    });

    ui.horizontal(|ui| {
        let stop_idle_clicked = ui
            .button("Stop idle VRMA")
            .on_hover_text(
                "Stop every AnimationPlayer (the idle VRMA sampler) so manual poses / animations stick.",
            )
            .clicked();
        if stop_idle_clicked {
            let mut n = 0usize;
            for mut player in players_q.iter_mut() {
                player.stop_all();
                n += 1;
            }
            state.status = Some(format!("stopped {n} AnimationPlayer(s)"));
        }
        let resume_clicked = ui
            .add_enabled(!vrma_entities.is_empty(), egui::Button::new("Resume idle VRMA"))
            .on_hover_text("Play the loaded idle VRMA on a loop again.")
            .clicked();
        if resume_clicked {
            for &e in vrma_entities {
                commands.trigger(PlayVrma {
                    vrma: e,
                    repeat: RepeatAnimation::Forever,
                    transition_duration: Duration::from_millis(300),
                    reset_spring_bones: true,
                });
            }
            state.status = Some(format!("resumed {} VRMA(s)", vrma_entities.len()));
        }
    });
    ui.checkbox(
        &mut pose_settings.auto_stop_idle_vrma,
        "Auto-stop idle VRMA on pose apply",
    )
    .on_hover_text(
        "When on, any manual pose / expression command stops every VRMA so the writes stick.",
    );

    ui.separator();
    ui.label(if let Some(name) = active_anim.current_name() {
        format!(
            "Playing (native): {name} — frame {:?}/{}",
            active_anim.current_frame(),
            active_anim.frame_count()
        )
    } else {
        "No native animation playing".into()
    });

    ui.separator();
    ui.collapsing("Snapshot current rig as pose", |ui| {
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut state.snapshot_name);
        });
        ui.horizontal(|ui| {
            ui.label("Category");
            ui.text_edit_singleline(&mut state.snapshot_category);
        });
        let enabled = snapshot.is_some() && !state.snapshot_name.trim().is_empty();
        if ui
            .add_enabled(enabled, egui::Button::new("Save snapshot"))
            .clicked()
        {
            if let Some(snap) = snapshot {
                let snap = snap.0.read().clone();
                let bones = snap
                    .bones
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            jarvis_avatar::pose_library::BoneRotation {
                                rotation: v.rotation,
                            },
                        )
                    })
                    .collect();
                let pose = PoseFile {
                    name: state.snapshot_name.trim().to_string(),
                    description: String::new(),
                    category: state.snapshot_category.trim().to_string(),
                    bones,
                    expressions: HashMap::new(),
                    transition_duration: 0.4,
                };
                match library.library.save_pose(&pose) {
                    Ok(path) => {
                        state.status = Some(format!("saved pose to {:?}", path));
                        library.mark_dirty();
                    }
                    Err(e) => state.status = Some(format!("save failed: {e}")),
                }
            }
        }
    });
}

// ---------- Library (poses) tab -----------------------------------------------

fn library_tab(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    sender: Option<&PoseCommandSender>,
) {
    ui.horizontal(|ui| {
        ui.label("Filter");
        ui.text_edit_singleline(&mut state.search);
        ui.label("Category");
        ui.text_edit_singleline(&mut state.category_filter);
        if ui.button("Refresh").clicked() {
            library.mark_dirty();
        }
    });

    let poses = library.poses();
    let search = state.search.trim().to_ascii_lowercase();
    let cat = state.category_filter.trim().to_ascii_lowercase();
    egui::ScrollArea::both()
        .max_height(ui.available_height() / 1.1)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for pose in poses {
                if !search.is_empty() && !pose.name.to_ascii_lowercase().contains(&search) {
                    continue;
                }
                if !cat.is_empty() && pose.category.to_ascii_lowercase() != cat {
                    continue;
                }
                pose_row(ui, state, library, sender, &pose);
            }
        });
}

fn pose_row(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    sender: Option<&PoseCommandSender>,
    pose: &PoseFile,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&pose.name).strong());
            ui.label(egui::RichText::new(format!("[{}]", pose.category)).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Apply").clicked() {
                    if let Some(s) = sender {
                        let bones = pose
                            .bones
                            .iter()
                            .map(|(k, v)| (k.clone(), v.rotation))
                            .collect();
                        s.send(PoseCommand::ApplyBones {
                            bones,
                            preserve_omitted_bones: true,
                            blend_weight: None,
                            transition_seconds: Some(pose.transition_duration),
                        });
                        if !pose.expressions.is_empty() {
                            s.send(PoseCommand::ApplyExpression {
                                weights: pose.expressions.clone(),
                                cancel_expression_animation: true,
                            });
                        }
                        state.status = Some(format!("applied {}", pose.name));
                    }
                }
                let rename_buf = state
                    .rename_buf
                    .entry(pose.name.clone())
                    .or_insert_with(|| pose.name.clone());
                ui.add(egui::TextEdit::singleline(rename_buf).desired_width(120.0));
                if ui.button("Rename").clicked() {
                    let new_name = rename_buf.trim().to_string();
                    if !new_name.is_empty() && new_name != pose.name {
                        match library.library.rename_pose(&pose.name, &new_name) {
                            Ok(()) => {
                                state.status = Some(format!("{} → {new_name}", pose.name));
                                library.mark_dirty();
                            }
                            Err(e) => state.status = Some(format!("rename failed: {e}")),
                        }
                    }
                }
                let cat_buf = state
                    .category_buf
                    .entry(pose.name.clone())
                    .or_insert_with(|| pose.category.clone());
                ui.add(egui::TextEdit::singleline(cat_buf).desired_width(90.0));
                if ui.button("Cat").on_hover_text("Change category").clicked() {
                    let cat = cat_buf.trim().to_string();
                    if !cat.is_empty() {
                        match library.library.update_pose_category(&pose.name, &cat) {
                            Ok(()) => {
                                state.status = Some(format!("{} category → {cat}", pose.name));
                                library.mark_dirty();
                            }
                            Err(e) => state.status = Some(format!("category failed: {e}")),
                        }
                    }
                }
                if ui.button("Delete").clicked() {
                    match library.library.delete_pose(&pose.name) {
                        Ok(()) => {
                            state.status = Some(format!("deleted {}", pose.name));
                            library.mark_dirty();
                        }
                        Err(e) => state.status = Some(format!("delete failed: {e}")),
                    }
                }
            });
        });
    });
}

// ---------- Animations tab ----------------------------------------------------

fn animations_tab(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    active_anim: &mut ResMut<ActiveNativeAnimation>,
    kimodo: Option<&KimodoClientRes>,
) {
    ui.horizontal(|ui| {
        ui.label("Filter");
        ui.text_edit_singleline(&mut state.search);
        ui.label("Category");
        ui.text_edit_singleline(&mut state.category_filter);
        ui.separator();
        ui.label("Default source:");
        ui.selectable_value(
            &mut state.default_playback_mode,
            PlaybackMode::Native,
            "Native",
        );
        ui.selectable_value(
            &mut state.default_playback_mode,
            PlaybackMode::Kimodo,
            "Kimodo peer",
        );
        if ui.button("Refresh").clicked() {
            library.mark_dirty();
        }
    });

    let anims = library.animations();
    let search = state.search.trim().to_ascii_lowercase();
    let cat = state.category_filter.trim().to_ascii_lowercase();
    egui::ScrollArea::both()
        .max_height(ui.available_height() / 1.1)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for meta in anims {
                if !search.is_empty() && !meta.name.to_ascii_lowercase().contains(&search) {
                    continue;
                }
                if !cat.is_empty() && meta.category.to_ascii_lowercase() != cat {
                    continue;
                }
                anim_row(ui, state, library, active_anim, kimodo, &meta);
            }
        });
}

fn anim_row(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    active_anim: &mut ResMut<ActiveNativeAnimation>,
    kimodo: Option<&KimodoClientRes>,
    meta: &AnimationMeta,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&meta.name).strong());
            ui.label(
                egui::RichText::new(format!(
                    "[{}] {}fr @{:.0} fps",
                    meta.category, meta.frame_count, meta.fps
                ))
                .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("▶ Native")
                    .on_hover_text("Play via Bevy native player")
                    .clicked()
                {
                    match library.library.load_animation(&meta.filename) {
                        Ok(anim) => {
                            let hold = meta.hold_duration;
                            active_anim.start(anim, meta.looping, hold);
                            state.status = Some(format!("native play {}", meta.name));
                        }
                        Err(e) => state.status = Some(format!("load failed: {e}")),
                    }
                }
                if ui
                    .button("▶ Kimodo")
                    .on_hover_text("Ask Kimodo peer to stream it")
                    .clicked()
                {
                    if let Some(k) = kimodo {
                        k.play_saved_animation(meta.filename.clone());
                        state.status = Some(format!("kimodo play {}", meta.name));
                    }
                }
                let rename_buf = state
                    .anim_rename_buf
                    .entry(meta.filename.clone())
                    .or_insert_with(|| meta.filename.clone());
                ui.add(egui::TextEdit::singleline(rename_buf).desired_width(130.0));
                if ui.button("Rename").clicked() {
                    let new_name = rename_buf.trim().to_string();
                    if !new_name.is_empty() && new_name != meta.filename {
                        let new_name = if new_name.ends_with(".json") {
                            new_name
                        } else {
                            format!("{new_name}.json")
                        };
                        match library.library.rename_animation(&meta.filename, &new_name) {
                            Ok(()) => {
                                state.status = Some(format!("{} → {new_name}", meta.filename));
                                library.mark_dirty();
                            }
                            Err(e) => state.status = Some(format!("rename failed: {e}")),
                        }
                    }
                }
                if ui.button("Delete").clicked() {
                    match library.library.delete_animation(&meta.filename) {
                        Ok(()) => {
                            state.status = Some(format!("deleted {}", meta.filename));
                            library.mark_dirty();
                        }
                        Err(e) => state.status = Some(format!("delete failed: {e}")),
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            let cat_buf = state
                .anim_category_buf
                .entry(meta.filename.clone())
                .or_insert_with(|| meta.category.clone());
            ui.label("Category");
            ui.add(egui::TextEdit::singleline(cat_buf).desired_width(110.0));
            let mut looping = meta.looping;
            let looping_before = looping;
            ui.checkbox(&mut looping, "Looping");
            let hold_buf = state
                .anim_hold_buf
                .entry(meta.filename.clone())
                .or_insert(meta.hold_duration);
            ui.label("Hold (s)");
            // Use a Slider rather than DragValue: egui 0.33's `smart_aim` has a
            // `debug_assert!` that can panic on certain drag deltas around 0.
            ui.add(egui::Slider::new(hold_buf, 0.0..=10.0).step_by(0.1));
            if ui.button("Save metadata").clicked() {
                let new_cat = cat_buf.trim().to_string();
                let new_hold = *hold_buf;
                match library.library.update_animation_metadata(
                    &meta.filename,
                    if new_cat.is_empty() {
                        None
                    } else {
                        Some(new_cat)
                    },
                    Some(looping),
                    Some(new_hold),
                ) {
                    Ok(()) => {
                        state.status = Some(format!("metadata saved for {}", meta.filename));
                        library.mark_dirty();
                    }
                    Err(e) => state.status = Some(format!("metadata failed: {e}")),
                }
            }
            // keep `looping_before` to silence warn about unused binding when
            // the checkbox value doesn't change.
            let _ = looping_before;
        });
    });
}

// ---------- AI Gen tab --------------------------------------------------------

fn ai_gen_tab(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    streaming: &StreamingAnimation,
    kimodo: Option<&KimodoClientRes>,
    tokio_rt: Option<&SharedTokio>,
) {
    ui.label("Describe the motion and stream it from the Kimodo peer:");
    ui.add(
        egui::TextEdit::multiline(&mut state.gen_prompt)
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    ui.horizontal(|ui| {
        ui.label("Duration (s)");
        ui.add(egui::Slider::new(&mut state.gen_duration, 0.5..=20.0).step_by(0.1));
        ui.label("Steps");
        ui.add(egui::Slider::new(&mut state.gen_steps, 10..=500));
        ui.checkbox(&mut state.gen_stream, "Stream frames");
    });
    ui.horizontal(|ui| {
        ui.label("Save as (optional)");
        ui.text_edit_singleline(&mut state.gen_save_name);
    });
    ui.horizontal(|ui| {
        let enabled = kimodo.is_some() && tokio_rt.is_some() && !state.gen_prompt.trim().is_empty();
        if ui
            .add_enabled(enabled, egui::Button::new("Generate"))
            .clicked()
        {
            if let (Some(k), Some(rt)) = (kimodo, tokio_rt) {
                let req = GenerateRequest {
                    prompt: state.gen_prompt.clone(),
                    duration: state.gen_duration,
                    steps: state.gen_steps,
                    stream: state.gen_stream,
                    save_name: if state.gen_save_name.trim().is_empty() {
                        None
                    } else {
                        Some(state.gen_save_name.trim().to_string())
                    },
                    timeout: std::time::Duration::from_secs(180),
                };
                let client = (*k).clone();
                state.status = Some("generate queued".into());
                // Spawn on the shared Tokio runtime so tokio::time / broadcast
                // receivers inside `generate_motion` have a reactor attached.
                // `futures::executor::block_on` on a bare thread lacks this
                // and panics the moment the future awaits anything tokio-ish.
                rt.spawn(async move {
                    match client.generate_motion(req).await {
                        Ok(out) => {
                            info!(
                                "kimodo generate finished: {} ({})",
                                out.final_status, out.final_message
                            );
                        }
                        Err(e) => warn!("kimodo generate failed: {e}"),
                    }
                });
                library.mark_dirty();
            }
        }
        if ui.button("Refresh list").clicked() {
            library.mark_dirty();
        }
    });

    ui.separator();
    ui.label(format!(
        "Streaming: active={} pending frames={}",
        streaming.active_request_id().is_some(),
        streaming.pending_frames()
    ));
}

// ---------- Idle tab ---------------------------------------------------------

fn idle_tab(
    ui: &mut egui::Ui,
    _state: &mut PoseControllerUiState,
    settings: &mut jarvis_avatar::config::PoseControllerSettings,
) {
    ui.checkbox(&mut settings.idle_enabled, "Enable local idle loop")
        .on_hover_text(
            "Periodically pick a random pose / animation from the filtered set and apply it.",
        );
    ui.horizontal(|ui| {
        ui.label("Min interval (s)");
        ui.add(egui::Slider::new(
            &mut settings.idle_interval_min_sec,
            1.0..=120.0,
        ));
        ui.label("Max interval (s)");
        ui.add(egui::Slider::new(
            &mut settings.idle_interval_max_sec,
            1.0..=300.0,
        ));
    });
    ui.horizontal(|ui| {
        ui.label("Category filter");
        ui.text_edit_singleline(&mut settings.idle_category);
    });
    ui.separator();
    ui.label("Blend / transition defaults (opt-in):");
    ui.checkbox(
        &mut settings.blend_transitions_enabled,
        "Honour blend_weight + transition_seconds",
    );
    ui.horizontal(|ui| {
        ui.label("Default transition (s)");
        ui.add(
            egui::Slider::new(&mut settings.default_transition_seconds, 0.0..=5.0).step_by(0.05),
        );
        ui.label("Default weight");
        ui.add(egui::Slider::new(&mut settings.default_blend_weight, 0.0..=1.0).step_by(0.05));
    });
}

// ---------- Expressions tab ----------------------------------------------------

fn send_expression_set(
    sender: Option<&PoseCommandSender>,
    presets: &[String],
    weights: &HashMap<String, f32>,
) {
    let Some(s) = sender else {
        return;
    };
    let mut m = HashMap::with_capacity(presets.len());
    for p in presets {
        m.insert(
            p.clone(),
            weights.get(p).copied().unwrap_or(0.0).clamp(0.0, 1.0),
        );
    }
    s.send(PoseCommand::SetExpression { weights: m });
}

fn expressions_tab(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    pose_settings: &jarvis_avatar::config::PoseControllerSettings,
) {
    ui.label(egui::RichText::new("VRM expression presets (VRMC_vrm)").strong());
    ui.label(format!(
        "Each slider is 0..=1. Every change sends `SetExpression` with **all** listed presets so the \
         face matches this panel. Idle VRMA can overwrite morphs unless **Auto-stop idle VRMA on pose apply** \
         is on in the Actions tab (currently {}).",
        if pose_settings.auto_stop_idle_vrma {
            "on"
        } else {
            "off — enable there if sliders seem to do nothing"
        }
    ));

    let presets: Vec<String> = snapshot
        .map(|h| h.0.read().expression_presets.clone())
        .unwrap_or_default();

    if state.expr_tracked_presets != presets {
        state.expr_tracked_presets = presets.clone();
        let old = std::mem::take(&mut state.expression_sliders);
        state.expression_sliders = presets
            .iter()
            .map(|p| (p.clone(), old.get(p).copied().unwrap_or(0.0)))
            .collect();
    }

    ui.horizontal(|ui| {
        if ui
            .button("Zero all")
            .on_hover_text("Set every weight to 0 and apply")
            .clicked()
        {
            for w in state.expression_sliders.values_mut() {
                *w = 0.0;
            }
            send_expression_set(sender, &state.expr_tracked_presets, &state.expression_sliders);
            state.status = Some("expressions: all zero".into());
        }
        if ui
            .button("Neutral @ 1")
            .on_hover_text("Zero all, then set `neutral` to 1.0 when this VRM defines that preset")
            .clicked()
        {
            for w in state.expression_sliders.values_mut() {
                *w = 0.0;
            }
            if let Some(w) = state.expression_sliders.get_mut("neutral") {
                *w = 1.0;
            }
            send_expression_set(sender, &state.expr_tracked_presets, &state.expression_sliders);
            state.status = Some("expressions: neutral".into());
        }
    });

    if presets.is_empty() {
        ui.add_space(6.0);
        ui.label(
            "No expression presets on the snapshot yet — wait for the VRM to finish loading, \
             or export VRMC_vrm expressions from Blender.",
        );
        return;
    }

    ui.add_space(4.0);
    ui.label(format!("{} preset(s).", presets.len()));
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for name in &presets {
            let w = state
                .expression_sliders
                .entry(name.clone())
                .or_insert(0.0);
            let response = ui.add(
                egui::Slider::new(w, 0.0..=1.0)
                    .text(name.as_str())
                    .step_by(0.01),
            );
            if response.changed() {
                *w = (*w).clamp(0.0, 1.0);
                send_expression_set(sender, &state.expr_tracked_presets, &state.expression_sliders);
                state.status = Some(format!("expression `{name}`"));
            }
        }
    });
}

// ---------- Bones tab --------------------------------------------------------

/// Diagnostic tab that writes directly to individual VRM bones so we can
/// confirm the pose-driver write path actually moves the visible armature.
///
/// Each slider drives one Euler angle (degrees, intrinsic XYZ, matching
/// Bevy's `Quat::from_euler(EulerRot::XYZ, …)`) and — on change — fires a
/// `PoseCommand::ApplyBones` containing just that bone. If the bone visibly
/// rotates when a slider moves, the write path is healthy; if it doesn't, the
/// write is landing on the wrong entity (phantom VRMA bone, retarget source,
/// etc.) and the fix belongs in `sync_bone_entity_index`.
fn bones_tab(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    indexed: Option<&IndexedBones>,
) {
    ui.label(egui::RichText::new("Manual bone controls (diagnostic)").strong());
    ui.label(
        "Drag a slider — the matching bone should rotate in real time. If a \
         bone refuses to move, its entity isn't in the bone index (see the \
         `pose_driver` info log) or our writes are being overwritten. Rows \
         shown in red aren't indexed on this rig, so no write reaches them.",
    );
    if let Some(idx) = indexed {
        let n_extra = idx
            .names
            .iter()
            .filter(|n| !is_vrm_humanoid_bone(n.as_str()))
            .count();
        let n_humanoid = idx.len().saturating_sub(n_extra);
        ui.label(format!(
            "Indexed: {} total — {n_humanoid} humanoid slot(s) + {n_extra} extra skin joint(s)",
            idx.len(),
        ));
        ui.label(
            "Extra rows use glTF node names (e.g. Rigify DEF-*). They share the same normalized pose quaternion space as humanoid bones (identity = bind); use Snapshot → sliders after reset.",
        );
    }

    ui.horizontal(|ui| {
        ui.label("Bone filter:");
        ui.add(
            egui::TextEdit::singleline(&mut state.bone_search)
                .hint_text("substring, e.g. toe / DEF- / leftHand")
                .desired_width((ui.available_width() - 90.0).clamp(120.0, 280.0)),
        );
        if ui.small_button("Clear").clicked() {
            state.bone_search.clear();
        }
    });
    let filter_lc = state.bone_search.trim().to_ascii_lowercase();

    ui.horizontal(|ui| {
        if ui
            .button("Reset to rest")
            .on_hover_text(
                "Snap every bone back to its VRM bind-pose local rotation (NOT identity — \
                 MMD-style rigs carry non-zero rest rotations on shoulders / twist bones, \
                 and resetting to identity produces a crossed-T shrug). Slider values are \
                 zeroed so your next drag starts from neutral; hit 'Snapshot → sliders' \
                 afterwards if you want the sliders to mirror the live rest rotations.",
            )
            .clicked()
        {
            state.bone_euler.clear();
            if let Some(s) = sender {
                s.send(PoseCommand::ResetPose);
            }
            state.status = Some("reset rig to bind pose".into());
        }
        if ui
            .button("Snapshot → sliders")
            .on_hover_text(
                "Seed the sliders with the current rig rotations so you can nudge from the live pose.",
            )
            .clicked()
        {
            if let Some(snap) = snapshot {
                let snap = snap.0.read().clone();
                state.bone_euler.clear();
                for (name, rot) in &snap.bones {
                    let q = Quat::from_xyzw(
                        rot.rotation[0],
                        rot.rotation[1],
                        rot.rotation[2],
                        rot.rotation[3],
                    );
                    let mut deg = euler_xyz_deg_intrinsic_stable_for_ui(q, Some(name.as_str()));
                    let yaw_extra = def_toe_big_yaw_slider_extra_deg(name);
                    // Only apply the big-toe yaw cosmetic when the row is not already the
                    // near-bind all-zero readout — otherwise (0,0,0) − 180° wrapped shows as Y=180.
                    if yaw_extra != 0.0 && deg.iter().any(|v| v.abs() > 0.5) {
                        deg[1] = wrap_deg_180_signed(deg[1] - yaw_extra);
                    }
                    state.bone_euler.insert(name.clone(), deg);
                }
                state.status = Some(format!(
                    "seeded {} bone slider(s) from live rig",
                    snap.bones.len()
                ));
            } else {
                state.status = Some("no snapshot yet".into());
            }
        }
        ui.label(format!("Tracked: {}", state.bone_euler.len()));
    });

    ui.horizontal(|ui| {
        if ui
            .button("Dump diagnostics to log")
            .on_hover_text(
                "Logs entity IDs, parent chain, and skin-joint membership for every indexed \
                 bone. Use this when a slider moves the bone's Transform but the avatar \
                 doesn't visibly react — the dump shows whether the entity we write is \
                 actually in the skinned mesh's joint list, and the ancestor chain from \
                 leftThumbProximal (known working) so you can compare against leftHand / \
                 leftUpperArm / shoulders / etc.",
            )
            .clicked()
        {
            if let Some(s) = sender {
                s.send(PoseCommand::DumpDiagnostics);
                state.status = Some("dumped bone diagnostics (see pose_driver log)".into());
            }
        }
    });

    ui.separator();

    egui::ScrollArea::vertical()
        .max_height(ui.available_height().max(200.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let grouped: HashSet<&str> = BONE_GROUPS
                .iter()
                .flat_map(|(_, b)| b.iter().copied())
                .collect();

            for (group_name, bones) in BONE_GROUPS {
                let filtered: Vec<&str> = bones
                    .iter()
                    .copied()
                    .filter(|b| bone_name_matches_search(&filter_lc, b))
                    .collect();
                if !filter_lc.is_empty() && filtered.is_empty() {
                    continue;
                }
                egui::CollapsingHeader::new(format!("{group_name} ({})", filtered.len()))
                    .id_salt(format!("bones-group-{group_name}"))
                    .default_open(false)
                    .show(ui, |ui| {
                        for bone in filtered {
                            bone_row(ui, state, sender, indexed, bone);
                        }
                    });
            }

            ui.add_space(8.0);

            let other_humanoid: Vec<&str> = VRM_BONE_NAMES
                .iter()
                .copied()
                .filter(|n| !grouped.contains(n) && bone_name_matches_search(&filter_lc, n))
                .collect();
            if !other_humanoid.is_empty() {
                egui::CollapsingHeader::new(format!(
                    "Other · standard humanoid ({})",
                    other_humanoid.len()
                ))
                .id_salt("bones-other-humanoid")
                .default_open(false)
                .show(ui, |ui| {
                    for bone in other_humanoid {
                        bone_row(ui, state, sender, indexed, bone);
                    }
                });
            }

            if let Some(idx) = indexed {
                let mut def_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
                let mut misc: Vec<String> = Vec::new();
                for n in &idx.names {
                    if is_vrm_humanoid_bone(n.as_str()) {
                        continue;
                    }
                    if !bone_name_matches_search(&filter_lc, n.as_str()) {
                        continue;
                    }
                    if let Some(cat) = def_bone_category_key(n.as_str()) {
                        def_groups.entry(cat).or_default().push(n.clone());
                    } else {
                        misc.push(n.clone());
                    }
                }
                for bones in def_groups.values_mut() {
                    bones.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
                }
                misc.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));

                for (cat_key, bones) in &def_groups {
                    if bones.is_empty() {
                        continue;
                    }
                    let title = format!(
                        "DEF · {} ({})",
                        format_def_category_title(cat_key),
                        bones.len()
                    );
                    egui::CollapsingHeader::new(title)
                        .id_salt(format!("bones-def-{cat_key}"))
                        .default_open(false)
                        .show(ui, |ui| {
                            for bone in bones {
                                bone_row(ui, state, sender, indexed, bone.as_str());
                            }
                        });
                }

                if !misc.is_empty() {
                    egui::CollapsingHeader::new(format!(
                        "Extra · other (non-DEF pattern) ({})",
                        misc.len()
                    ))
                    .id_salt("bones-extra-misc")
                    .default_open(false)
                    .show(ui, |ui| {
                        for bone in &misc {
                            bone_row(ui, state, sender, indexed, bone.as_str());
                        }
                    });
                }
            }
        });
}

fn bone_row(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    indexed: Option<&IndexedBones>,
    bone: &str,
) {
    let euler = state
        .bone_euler
        .entry(bone.to_string())
        .or_insert([0.0, 0.0, 0.0]);
    let mut x = euler[0];
    let mut y = euler[1];
    let mut z = euler[2];

    let in_index = indexed.is_some_and(|i| i.contains(bone));

    let mut changed = false;
    let mut reset_to_rest = false;
    ui.horizontal(|ui| {
        let label = egui::RichText::new(bone).monospace();
        let label = if in_index {
            label
        } else {
            label.color(egui::Color32::from_rgb(220, 110, 110))
        };
        ui.label(label);
        if !in_index {
            ui.label(
                egui::RichText::new("(not indexed)")
                    .small()
                    .color(egui::Color32::from_rgb(220, 110, 110)),
            )
            .on_hover_text(if is_vrm_humanoid_bone(bone) {
                "This VRM doesn't expose an entity for this humanoid bone — writes are silently dropped."
            } else {
                "This bone isn't in the pose driver's merged index (not a named joint in this avatar's SkinnedMesh list, or only exists under a VRMA asset) — writes are silently dropped."
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("↺")
                .on_hover_text(
                    "Snap this bone back to its full VRM bind `Transform` from `RestTransform` \
                     (translation + rotation + scale), zero the sliders, then re-apply the \
                     same normalized pose_q the sliders use at (0°,0°,0°) so rotation matches \
                     manual apply — avoids DEF-toe_big and other binds where raw rest rotation \
                     differs from slider-neutral pose.",
                )
                .clicked()
            {
                x = 0.0;
                y = 0.0;
                z = 0.0;
                reset_to_rest = true;
            }
        });
    });
    ui.horizontal(|ui| {
        ui.label("X");
        if ui
            .add(egui::Slider::new(&mut x, -180.0..=180.0).suffix("°"))
            .changed()
        {
            changed = true;
        }
        ui.label("Y");
        if ui
            .add(egui::Slider::new(&mut y, -180.0..=180.0).suffix("°"))
            .changed()
        {
            changed = true;
        }
        ui.label("Z");
        if ui
            .add(egui::Slider::new(&mut z, -180.0..=180.0).suffix("°"))
            .changed()
        {
            changed = true;
        }
    });

    if reset_to_rest {
        euler[0] = 0.0;
        euler[1] = 0.0;
        euler[2] = 0.0;
        if let Some(s) = sender {
            // Full bind (translation + rotation + scale) — needed for Rigify DEF-*.
            s.send(PoseCommand::ResetBones(vec![bone.to_string()]));
            // `ResetBones` copies raw `RestTransform`; slider "zero" uses normalized pose_q
            // → `local_from_normalized` (see pose_driver). Those can differ on Helen
            // `DEF-toe_{big,index,middle,ring,little}` (±180° display-yaw cosmetic) and any
            // bind where file rest rotation ≠ slider-neutral pose.
            // Re-apply the same quaternion the sliders would send for (0°,0°,0°) so ↺ matches
            // a manual zero apply without flipping.
            send_apply_bones_euler_deg(s, bone, [0.0, 0.0, 0.0]);
            state.status = Some(format!("{bone} → rest"));
        }
    } else if changed {
        euler[0] = x;
        euler[1] = y;
        euler[2] = z;
        if let Some(s) = sender {
            send_apply_bones_euler_deg(s, bone, [x, y, z]);
            state.status = Some(format!("{bone} → ({x:.1}, {y:.1}, {z:.1})"));
        }
    }
    ui.separator();
}
