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
//! [`crate::pose_library::PoseLibrary`]); disk mutations bubble the
//! refresh cache automatically.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use bevy::animation::RepeatAnimation;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_egui::egui::Layout;
use bevy_egui::{EguiContexts, egui};
use bevy_vrm1::prelude::*;

use crate::config::Settings;
use crate::icons;
use crate::pose_library::{AnimationMeta, PoseFile};
use crate::theme;

use crate::mcp::pose_authoring::{bone_map_from_euler_deg, sanitize_bone_map, BoneEulerDeg};
use crate::mcp::pose_intents::{
    compile_arms_down_rest, compile_bend_knee, compile_raise_leg, ArmsDownRestArgs,
    BendKneeArgs, LegRaiseDirection, RaiseLegArgs, Side,
};
use crate::mcp::pose_safety::PoseSafetyReport;
use crate::mcp::semantic_intent_calibration::SemanticIntentCalibration;
use crate::kimodo::{GenerateRequest, KimodoClient};
use crate::plugins::anim_layer_sets::LayerSetsStore;
use crate::plugins::anim_layers::{
    LayerStackHandle, RestPoseSnapshot, animation_edit_layer_set_name,
    bake_layer_stack_to_animation, begin_library_animation_edit,
};
use crate::plugins::bone_influence::BoneInfluence;
use crate::plugins::material_visibility::MaterialVisibilityStore;
use crate::plugins::native_anim_player::{ActiveNativeAnimation, StreamingAnimation};
use crate::plugins::pose_driver::{
    BoneHierarchy, IndexedBones, PoseCommand, PoseCommandSender, VRM_BONE_NAMES,
    def_toe_big_yaw_slider_extra_deg, is_vrm_humanoid_bone,
};
use crate::plugins::intent_calibration::{
    IntentCalibrationWizardHandle, SemanticIntentCalibrationHandle,
};
use crate::mcp::intent_calibration_wizard::{ConfirmVerdict, WIZARD_STEPS, WizardPhase};
use crate::plugins::pose_library_assets::PoseLibraryAssets;
use crate::plugins::rig_editor::{HoverSource, RigEditAxis};
use crate::plugins::shared_runtime::SharedTokio;
use crate::plugins::undo_history::UndoHistory;
use crate::plugins::vrm_preset_key;

/// Layer-stack resources for the Animation tab (keeps `draw_pose_controller_window`
/// under Bevy's system-param limit).
#[derive(SystemParam)]
pub struct AnimationLayerEditUiParams<'w> {
    pub stack: Option<Res<'w, LayerStackHandle>>,
    pub layer_sets: Option<Res<'w, LayerSetsStore>>,
    pub rest: Option<Res<'w, RestPoseSnapshot>>,
    pub hierarchy: Option<Res<'w, BoneHierarchy>>,
}

/// Bones-tab resources bundled to keep `draw_pose_controller_window` under
/// Bevy's 16 system-param limit. `indexed` is the writable bone set every tab
/// reads; `influence` + `material_vis` drive the Bones tab's non-deforming /
/// hidden-mesh declutter filters.
#[derive(SystemParam)]
pub struct BonesPanelData<'w> {
    pub indexed: Option<Res<'w, IndexedBones>>,
    pub influence: Option<Res<'w, BoneInfluence>>,
    pub material_vis: Option<Res<'w, MaterialVisibilityStore>>,
    pub director_state: Option<ResMut<'w, crate::plugins::alive_director::DirectorState>>,
    pub director_status: Option<Res<'w, crate::plugins::alive_director::DirectorStatus>>,
}

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

/// Semantic group for a non-humanoid, non-DEF extra bone (`hair_*`, `skirt_*`,
/// `waistBelt_*`, `toe_*`, …). First substring match wins.
fn extra_bone_category(name: &str) -> &'static str {
    let l = name.to_ascii_lowercase();
    const TABLE: &[(&str, &str)] = &[
        ("hair", "Hair"),
        ("headband", "Headband"),
        ("ribbon", "Ribbon"),
        ("sleeve", "Sleeve"),
        ("skirt", "Skirt"),
        ("waistbelt", "Belt"),
        ("belt", "Belt"),
        ("waist", "Waist"),
        ("breast", "Breast"),
        ("glute", "Glute"),
        ("toe", "Toes"),
        ("eye", "Eyes"),
        ("heart", "Heart"),
        ("twist", "Twist"),
        ("shoulder", "Shoulder"),
    ];
    for (pat, label) in TABLE {
        if l.contains(pat) {
            return label;
        }
    }
    "Other"
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
/// Public to the crate so the Rig tab's axis-drag and slider can route through
/// the *exact* same write path (DEF-toe yaw cosmetic included), keeping the
/// two surfaces in sync.
pub(crate) fn send_apply_bones_euler_deg(sender: &PoseCommandSender, bone: &str, deg: [f32; 3]) {
    send_apply_bones_euler_deg_mirrored(sender, bone, deg, None)
}

/// Variant that also sends the mirrored counterpart when `mirror` is in
/// realtime mode and the bone has a partner. Routes the mirror through the
/// same single `ApplyBones` event as the primary write so retarget / spring
/// reset / animation-layer accumulation see one atomic update per drag tick.
pub(crate) fn send_apply_bones_euler_deg_mirrored(
    sender: &PoseCommandSender,
    bone: &str,
    deg: [f32; 3],
    mirror: Option<&crate::plugins::mirror::MirrorState>,
) {
    let yaw_extra = def_toe_big_yaw_slider_extra_deg(bone);
    let q = Quat::from_euler(
        EulerRot::XYZ,
        deg[0].to_radians(),
        (deg[1] + yaw_extra).to_radians(),
        deg[2].to_radians(),
    );
    let mut bones = HashMap::new();
    bones.insert(bone.to_string(), [q.x, q.y, q.z, q.w]);

    if let Some(m) = mirror {
        if m.realtime {
            let result = m.expand(bone, [q.x, q.y, q.z, q.w]);
            // `expand` already includes the primary; replace our map so the
            // mirror partner is added without double-inserting the primary.
            bones = result.bones;
        }
    }

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
    pub anim_loop_buf: HashMap<String, bool>,
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
    /// Bones tab: reveal bones that move no geometry (toe `_03` tips, hair/ribbon
    /// tips). Off by default — those are hidden to declutter the extra-bone list.
    pub bones_show_non_deforming: bool,
    /// Bones tab: hide bones whose influenced meshes are all currently hidden
    /// (e.g. a belt bone once its material is hidden). On by default.
    pub bones_hide_hidden_mesh: bool,
    /// Last `expression_presets` list from the live VRM; when it changes, [`Self::expression_sliders`]
    /// is rebuilt (weights preserved for names that still exist).
    pub expr_tracked_presets: Vec<String>,
    /// 0..=1 weights for the **Expressions** tab; keys are VRMC_vrm preset names.
    pub expression_sliders: HashMap<String, f32>,
    // --- Intent Lab (semantic MCP calibration) ---
    pub intent_lab_sync_key: String,
    pub intent_lab_cal: SemanticIntentCalibration,
    pub intent_lab_raise_amount: f32,
    pub intent_lab_bend_amount: f32,
    pub intent_lab_arms_amount: f32,
    /// Test raise_leg on the left vs right leg.
    pub intent_lab_side_left: bool,
    /// `true` = forward (hip flex), `false` = outward (abduction roll).
    pub intent_lab_raise_forward: bool,
    /// Last "Mirror chain → …" message — surfaced next to the toggle so the
    /// user sees which side they just mirrored.
    pub mirror_chain_status: Option<String>,
    /// Pose name currently in inline-rename mode (click the title cell of a
    /// pose row to enter rename mode; save / cancel commit and clear it).
    pub renaming_pose: Option<String>,
    /// Pose name currently in "edit" mode (row-click toggles this on after
    /// applying, so the user can change category / delete via inline icons
    /// without crowding every row).
    pub editing_pose: Option<String>,
    /// Animation file currently in inline-rename mode.
    pub renaming_animation: Option<String>,
    /// Animation file currently in edit mode (layer-stack authoring).
    pub editing_animation: Option<String>,
    /// When set, the animation tab is driving playback through the layer stack
    /// and Done will bake + save both the library clip and layer-set preset.
    pub animation_layer_edit: Option<AnimationLayerEditSession>,
    /// "New category" creation form: pose-name → user-typed new category.
    /// When `Some`, the row's category combobox shows a text input + save
    /// instead of the dropdown.
    pub new_category_buf: HashMap<String, String>,
    /// Per-side active tab tracker. Maps a panel side string (`"left"`,
    /// `"right"`, `"bottom"`) to whichever tab is currently focused inside
    /// that side panel. Transient — not persisted to user.toml; the user's
    /// last active tab on each side resets to the first tab assigned there
    /// when the app restarts.
    pub per_side_active: HashMap<String, PoseControllerTab>,
}

impl Default for PoseControllerUiState {
    fn default() -> Self {
        Self {
            tab: PoseControllerTab::Library,
            search: String::new(),
            category_filter: String::new(),
            status: None,
            rename_buf: HashMap::new(),
            category_buf: HashMap::new(),
            anim_rename_buf: HashMap::new(),
            anim_category_buf: HashMap::new(),
            anim_hold_buf: HashMap::new(),
            anim_loop_buf: HashMap::new(),
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
            bones_show_non_deforming: false,
            bones_hide_hidden_mesh: true,
            expr_tracked_presets: Vec::new(),
            expression_sliders: HashMap::new(),
            intent_lab_sync_key: String::new(),
            intent_lab_cal: SemanticIntentCalibration::default(),
            intent_lab_raise_amount: 0.35,
            intent_lab_bend_amount: 0.35,
            intent_lab_arms_amount: 0.85,
            intent_lab_side_left: true,
            intent_lab_raise_forward: true,
            mirror_chain_status: None,
            renaming_pose: None,
            editing_pose: None,
            renaming_animation: None,
            editing_animation: None,
            animation_layer_edit: None,
            new_category_buf: HashMap::new(),
            per_side_active: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AnimationLayerEditSession {
    pub filename: String,
    pub layer_set_name: String,
    /// Stack snapshot taken before edit — restored on Cancel.
    pub stack_backup: crate::plugins::anim_layers::LayerStack,
}

/// Resources needed by the Animation tab for layer-stack editing.
struct AnimationTabLayerContext<'a> {
    stack: Option<&'a LayerStackHandle>,
    layer_sets: Option<&'a LayerSetsStore>,
    rest: Option<&'a RestPoseSnapshot>,
    hierarchy: Option<&'a BoneHierarchy>,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum PoseControllerTab {
    /// Pose library + snapshot + edit-mode rename/category combobox.
    /// (Was `Library`; renamed since "Poses" is the user-visible label and the
    /// snapshot UI now lives here too.)
    Library,
    /// Merged Animations + AI Gen + Idle workspace. Animations are grouped by
    /// category in a single explorer; the right side has the generator + idle
    /// controls. Replaces the old separate `Animations`, `AiGen`, `Idle` tabs.
    Animation,
    /// Bones list (left) + Expressions sliders (right) split layout.
    /// Replaces the standalone `Bones` and `Expressions` tabs.
    Bones,
    /// Rig editor tab — viewport hover/select, RGB axis rings, drag-to-rotate,
    /// VRMC spring tuning, **plus** the new mirror panel.
    Rig,
    IntentLab,
}

impl PoseControllerTab {
    /// Stable string key for persisting per-tab UI state (which tabs the user
    /// has popped into floating windows, etc.) into `config/user.toml`.
    pub fn config_key(self) -> &'static str {
        match self {
            PoseControllerTab::Library => "library",
            PoseControllerTab::Animation => "animation",
            PoseControllerTab::Bones => "bones",
            PoseControllerTab::Rig => "rig",
            PoseControllerTab::IntentLab => "intent_lab",
        }
    }

    pub fn from_config_key(key: &str) -> Option<Self> {
        Some(match key {
            "library" => PoseControllerTab::Library,
            "animation" => PoseControllerTab::Animation,
            "bones" => PoseControllerTab::Bones,
            "rig" => PoseControllerTab::Rig,
            "intent_lab" => PoseControllerTab::IntentLab,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            PoseControllerTab::Library => "Poses",
            PoseControllerTab::Animation => "Animation",
            PoseControllerTab::Bones => "Bones + Expressions",
            PoseControllerTab::Rig => "Rig",
            PoseControllerTab::IntentLab => "Intent Lab",
        }
    }

    pub fn all() -> [PoseControllerTab; 5] {
        [
            PoseControllerTab::Library,
            PoseControllerTab::Animation,
            PoseControllerTab::Bones,
            PoseControllerTab::Rig,
            PoseControllerTab::IntentLab,
        ]
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum PlaybackMode {
    /// Frame-driven by the native Bevy player (reads `AnimationFile.frames`).
    Native,
    /// Forwards `kimodo:play-animation` so the Python peer streams poses back.
    Kimodo,
}

pub fn draw_pose_controller_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    library: Option<Res<PoseLibraryAssets>>,
    sender: Option<Res<PoseCommandSender>>,
    mut active_anim: ResMut<ActiveNativeAnimation>,
    streaming: Res<StreamingAnimation>,
    kimodo_client: Option<Res<KimodoClientRes>>,
    tokio_rt: Option<Res<SharedTokio>>,
    snapshot: Option<Res<crate::plugins::pose_driver::BoneSnapshotHandle>>,
    mut bones_data: BonesPanelData,
    intent_cal: Option<Res<SemanticIntentCalibrationHandle>>,
    intent_wizard: Option<Res<IntentCalibrationWizardHandle>>,
    undo: Res<UndoHistory>,
    mut state: ResMut<super::DebugUiState>,
    mut rig_params: super::rig_editor::RigTabSystemParam,
    layer_edit: AnimationLayerEditUiParams,
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

    // Bones-tab resources (bundled in `bones_data` to stay under the system-param
    // limit) — borrowed once and threaded to the Bones panel alongside `indexed`.
    let indexed = bones_data.indexed.as_deref();
    let bone_influence = bones_data.influence.as_deref();
    let material_vis = bones_data.material_vis.as_deref();

    // Note: the always-visible transport buttons (Reset / Stop native / Stop
    // idle / Resume idle) and the rig hover hint (`[edit] hover: …
    // selected: … axis: …`) live on the global menu bar — see
    // `super::draw_menu_bar`. The Vrma / AnimationPlayer / Commands queries
    // they used to rely on now belong to that system. The Pose Tools toolbar
    // (edit-mode / axis / mirror / per-panel show-hide) sits immediately
    // below the menu bar and is rendered by
    // [`super::pose_tools_toolbar::draw_pose_tools_toolbar`].

    // Resolve per-tab dock side. Tabs without an override fall back to
    // `pose_controller_dock_side` (the legacy single-side knob acts as the
    // default). Tabs marked `"hidden"` render nowhere — the user can re-show
    // them from the panel toggles in the global toolbar.
    let undocked_legacy: Vec<PoseControllerTab> = settings
        .ui
        .pose_controller_undocked_tabs
        .iter()
        .filter_map(|s| PoseControllerTab::from_config_key(s))
        .collect();

    let default_side = settings.ui.pose_controller_dock_side.clone();
    let mut tab_sides: HashMap<PoseControllerTab, String> = HashMap::new();
    for tab in PoseControllerTab::all() {
        let key = tab.config_key();
        let side = if undocked_legacy.contains(&tab) {
            "floating".to_string()
        } else {
            settings
                .ui
                .pose_controller_tab_dock_sides
                .get(key)
                .cloned()
                .unwrap_or_else(|| default_side.clone())
        };
        tab_sides.insert(tab, side);
    }

    // The Bones tab gets force-focused whenever a viewport pick (or another
    // surface) sets `pending_scroll_to_bone`. That means we need to make sure
    // the Bones tab is reachable: if it's currently `hidden`, surface it on
    // its panel side so the user actually sees the scroll target.
    if rig_params.rig.pending_scroll_to_bone.is_some() {
        let bones_side = tab_sides
            .get(&PoseControllerTab::Bones)
            .cloned()
            .unwrap_or_else(|| default_side.clone());
        if bones_side == "hidden" {
            tab_sides.insert(PoseControllerTab::Bones, default_side.clone());
        }
        state
            .pose_controller
            .per_side_active
            .insert(bones_side.clone(), PoseControllerTab::Bones);
    }

    // Group tabs by side so each side renders one panel with that side's tab
    // bar.
    let mut side_tabs: HashMap<String, Vec<PoseControllerTab>> = HashMap::new();
    for tab in PoseControllerTab::all() {
        let side = tab_sides[&tab].clone();
        if side == "hidden" {
            continue;
        }
        side_tabs.entry(side).or_default().push(tab);
    }

    // Persistable mutations queued during render — applied after the egui
    // panels close so we never mutate `settings` while it's borrowed by the
    // render closures.
    let mut pending_side_changes: Vec<(PoseControllerTab, &'static str)> = Vec::new();
    let mut pending_panel_width: Option<f32> = None;
    let mut pending_panel_height: Option<f32> = None;

    let layer_ctx = AnimationTabLayerContext {
        stack: layer_edit.stack.as_deref(),
        layer_sets: layer_edit.layer_sets.as_deref(),
        rest: layer_edit.rest.as_deref(),
        hierarchy: layer_edit.hierarchy.as_deref(),
    };

    let dock_width = settings.ui.pose_controller_dock_width.max(280.0);
    let dock_height = settings.ui.pose_controller_dock_bottom_height.max(180.0);

    let mut dock = std::mem::take(&mut state.dock);
    let mut root = dock.begin(ctx);

    // Order matters for egui SidePanel/BottomPanel rendering: do bottom first
    // so it claims the bottom strip across the full window width, then the
    // left/right side panels render above it.
    if let Some(tabs) = side_tabs.get("bottom").cloned() {
        let resp = egui::Panel::bottom("pose_controller_bottom_panel")
            .resizable(true)
            .default_size(dock_height)
            .min_size(180.0)
            .show(&mut root, |ui| {
                render_side_panel(
                    ui,
                    "bottom",
                    &tabs,
                    &mut state,
                    &library,
                    sender.as_deref(),
                    &mut active_anim,
                    &streaming,
                    kimodo_client.as_deref(),
                    tokio_rt.as_deref(),
                    snapshot.as_deref(),
                    indexed,
                    bone_influence,
                    material_vis,
                    &mut rig_params,
                    &mut settings,
                    intent_cal.as_deref(),
                    intent_wizard.as_deref(),
                    Some(&*undo),
                    &mut pending_side_changes,
                    &layer_ctx,
                    (
                        bones_data.director_state.as_mut().map(|r| &mut **r),
                        bones_data.director_status.as_deref(),
                    ),
                );
            });
        pending_panel_height = Some(resp.response.rect.height());
    }
    if let Some(tabs) = side_tabs.get("left").cloned() {
        let resp = egui::Panel::left("pose_controller_left_panel")
            .resizable(true)
            .default_size(dock_width)
            .min_size(320.0)
            .max_size(1100.0)
            .show(&mut root, |ui| {
                render_side_panel(
                    ui,
                    "left",
                    &tabs,
                    &mut state,
                    &library,
                    sender.as_deref(),
                    &mut active_anim,
                    &streaming,
                    kimodo_client.as_deref(),
                    tokio_rt.as_deref(),
                    snapshot.as_deref(),
                    indexed,
                    bone_influence,
                    material_vis,
                    &mut rig_params,
                    &mut settings,
                    intent_cal.as_deref(),
                    intent_wizard.as_deref(),
                    Some(&*undo),
                    &mut pending_side_changes,
                    &layer_ctx,
                    (
                        bones_data.director_state.as_mut().map(|r| &mut **r),
                        bones_data.director_status.as_deref(),
                    ),
                );
            });
        pending_panel_width = Some(resp.response.rect.width());
    }
    if let Some(tabs) = side_tabs.get("right").cloned() {
        let resp = egui::Panel::right("pose_controller_right_panel")
            .resizable(true)
            .default_size(dock_width)
            .min_size(320.0)
            .max_size(1100.0)
            .show(&mut root, |ui| {
                render_side_panel(
                    ui,
                    "right",
                    &tabs,
                    &mut state,
                    &library,
                    sender.as_deref(),
                    &mut active_anim,
                    &streaming,
                    kimodo_client.as_deref(),
                    tokio_rt.as_deref(),
                    snapshot.as_deref(),
                    indexed,
                    bone_influence,
                    material_vis,
                    &mut rig_params,
                    &mut settings,
                    intent_cal.as_deref(),
                    intent_wizard.as_deref(),
                    Some(&*undo),
                    &mut pending_side_changes,
                    &layer_ctx,
                    (
                        bones_data.director_state.as_mut().map(|r| &mut **r),
                        bones_data.director_status.as_deref(),
                    ),
                );
            });
        // `pending_panel_width` only tracks the most recently rendered side
        // panel; both sides share the same persisted dock width so the user
        // doesn't need separate config knobs per side. Last-resize wins.
        pending_panel_width = Some(resp.response.rect.width());
    }

    dock.end(&root);
    state.dock = dock;

    // Floating tabs: each gets its own egui::Window.
    if let Some(tabs) = side_tabs.get("floating").cloned() {
        for tab in tabs {
            let mut keep_open = true;
            egui::Window::new(format!("Pose · {}", tab.label()))
                .id(egui::Id::new(("pose_controller_floating", tab.config_key())))
                .default_size([520.0, 540.0])
                .open(&mut keep_open)
                .show(ctx, |ui| {
                    floating_tab_header(ui, tab, &mut pending_side_changes);
                    ui.separator();
                    render_tab_body(
                        ui,
                        tab,
                        &mut state.pose_controller,
                        &library,
                        sender.as_deref(),
                        &mut active_anim,
                        &streaming,
                        kimodo_client.as_deref(),
                        tokio_rt.as_deref(),
                        snapshot.as_deref(),
                        indexed,
                        bone_influence,
                        material_vis,
                        &mut rig_params,
                        &mut settings,
                        intent_cal.as_deref(),
                        intent_wizard.as_deref(),
                        Some(&*undo),
                        &layer_ctx,
                        (
                            bones_data.director_state.as_mut().map(|r| &mut **r),
                            bones_data.director_status.as_deref(),
                        ),
                    );
                });
            if !keep_open {
                // Close = redock to the workspace default.
                pending_side_changes.push((tab, "default"));
            }
        }
    }

    // Apply queued mutations now that all render scopes have closed.
    for (tab, target) in pending_side_changes {
        let key = tab.config_key().to_string();
        if target == "default" {
            settings.ui.pose_controller_tab_dock_sides.remove(&key);
            settings
                .ui
                .pose_controller_undocked_tabs
                .retain(|s| s != &key);
        } else {
            settings
                .ui
                .pose_controller_tab_dock_sides
                .insert(key.clone(), target.to_string());
            // Legacy float-list mirror so older user.toml installs that still
            // read it stay consistent — added only when target == floating.
            if target == "floating" {
                if !settings.ui.pose_controller_undocked_tabs.contains(&key) {
                    settings.ui.pose_controller_undocked_tabs.push(key);
                }
            } else {
                settings
                    .ui
                    .pose_controller_undocked_tabs
                    .retain(|s| s != &key);
            }
        }
    }
    if let Some(w) = pending_panel_width {
        if (w - settings.ui.pose_controller_dock_width).abs() > 1.0 {
            settings.ui.pose_controller_dock_width = w;
        }
    }
    if let Some(h) = pending_panel_height {
        if (h - settings.ui.pose_controller_dock_bottom_height).abs() > 1.0 {
            settings.ui.pose_controller_dock_bottom_height = h;
        }
    }
}

/// Render one side panel containing the tab bar + active body for the tabs
/// docked to `side`. Bones-side rendering also surfaces the per-tab
/// "Send to ▼" menu and a quick "hide" button for users who want to clear
/// the panel without going through the View menu.
#[allow(clippy::too_many_arguments)]
fn render_side_panel(
    ui: &mut egui::Ui,
    side: &str,
    tabs: &[PoseControllerTab],
    state: &mut super::DebugUiState,
    library: &PoseLibraryAssets,
    sender: Option<&PoseCommandSender>,
    active_anim: &mut ResMut<ActiveNativeAnimation>,
    streaming: &StreamingAnimation,
    kimodo_client: Option<&KimodoClientRes>,
    tokio_rt: Option<&SharedTokio>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    indexed: Option<&IndexedBones>,
    bone_influence: Option<&BoneInfluence>,
    material_vis: Option<&MaterialVisibilityStore>,
    rig_params: &mut super::rig_editor::RigTabSystemParam,
    settings: &mut Settings,
    intent_cal: Option<&SemanticIntentCalibrationHandle>,
    intent_wizard: Option<&IntentCalibrationWizardHandle>,
    undo: Option<&UndoHistory>,
    pending_side_changes: &mut Vec<(PoseControllerTab, &'static str)>,
    layer_ctx: &AnimationTabLayerContext<'_>,
    director: (
        Option<&mut crate::plugins::alive_director::DirectorState>,
        Option<&crate::plugins::alive_director::DirectorStatus>,
    ),
) {
    if tabs.is_empty() {
        ui.label(egui::RichText::new("(no tabs assigned to this panel)").italics());
        return;
    }

    let pc = &mut state.pose_controller;

    // Resolve the active tab on this side, defaulting to the first assigned
    // tab. We also clamp if the previously-active tab has since moved away.
    let mut active = pc
        .per_side_active
        .get(side)
        .copied()
        .filter(|t| tabs.contains(t))
        .unwrap_or(tabs[0]);

    side_panel_tab_bar(ui, side, tabs, &mut active, pending_side_changes);
    pc.per_side_active.insert(side.to_string(), active);
    // Mirror the side-active tab into `pc.tab` so legacy paths that still
    // look at "the" active tab (Rig hover hint, Bones scroll snap) stay
    // consistent with whatever the user is looking at most recently.
    pc.tab = active;
    ui.separator();

    if rig_params.rig.pending_scroll_to_bone.is_some() && active != PoseControllerTab::Bones {
        // Scroll target landed on a panel that doesn't host Bones — clear it
        // so we don't keep flipping tabs every frame.
        rig_params.rig.pending_scroll_to_bone = None;
    }

    render_tab_body(
        ui,
        active,
        pc,
        library,
        sender,
        active_anim,
        streaming,
        kimodo_client,
        tokio_rt,
        snapshot,
        indexed,
        bone_influence,
        material_vis,
        rig_params,
        settings,
        intent_cal,
        intent_wizard,
        undo,
        layer_ctx,
        director,
    );

    egui::Panel::bottom(format!("pose_controller_{side}_status_strip")).show(
        ui,
        |ui| {
            ui.horizontal(|ui| {
                if let Some(msg) = &pc.status {
                    ui.label(msg);
                    ui.separator();
                }
                ui.with_layout(Layout::right_to_left(egui::Align::Max), |ui| {
                    if let Some(err) = library.last_error() {
                        ui.colored_label(theme::error(ui), err);
                    }
                });
            });
        },
    );
}

/// Tab bar for one side panel. Each tab row is `[ Tab ] [ ... ]` — clicking
/// the tab activates it; clicking the trailing menu opens a "Move to side"
/// picker. Plain ASCII labels are used everywhere so glyphs render in the
/// default egui font (no missing-emoji squares).
fn side_panel_tab_bar(
    ui: &mut egui::Ui,
    current_side: &str,
    tabs: &[PoseControllerTab],
    active: &mut PoseControllerTab,
    pending_side_changes: &mut Vec<(PoseControllerTab, &'static str)>,
) {
    ui.horizontal_wrapped(|ui| {
        for tab in tabs {
            if ui
                .selectable_label(*active == *tab, tab.label())
                .clicked()
            {
                *active = *tab;
            }
            ui.menu_button("...", |ui| {
                ui.label(egui::RichText::new("Move to...").strong());
                ui.separator();
                let mut send = |ui: &mut egui::Ui, label: String, target: &'static str| {
                    let enabled = current_side != target;
                    if ui
                        .add_enabled(enabled, egui::Button::new(label))
                        .clicked()
                    {
                        pending_side_changes.push((*tab, target));
                        ui.close();
                    }
                };
                send(ui, format!("{} Left side panel", icons::DOCK_LEFT), "left");
                send(ui, format!("{} Right side panel", icons::DOCK_RIGHT), "right");
                send(ui, format!("{} Bottom panel (dopesheet)", icons::DOCK_BOTTOM), "bottom");
                send(ui, format!("{} Floating window", icons::FLOATING), "floating");
                ui.separator();
                if ui.button(format!("{} Hide tab", icons::HIDDEN)).clicked() {
                    pending_side_changes.push((*tab, "hidden"));
                    ui.close();
                }
                if ui.button(format!("{} Reset to default side", icons::REVERSE)).clicked() {
                    pending_side_changes.push((*tab, "default"));
                    ui.close();
                }
            });
            ui.separator();
        }
    });
}

/// Tiny header strip rendered inside each floating-tab window. Carries a
/// "Redock" menu so the user can move a popped-out tab back into a side
/// panel without going through the global toolbar.
fn floating_tab_header(
    ui: &mut egui::Ui,
    tab: PoseControllerTab,
    pending_side_changes: &mut Vec<(PoseControllerTab, &'static str)>,
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("[{}] floating window", tab.label()))
                .small()
                .color(egui::Color32::from_rgb(180, 200, 230)),
        );
        ui.separator();
        ui.menu_button("Redock", |ui| {
            if ui.button(format!("{} Left side panel", icons::DOCK_LEFT)).clicked() {
                pending_side_changes.push((tab, "left"));
                ui.close();
            }
            if ui.button(format!("{} Right side panel", icons::DOCK_RIGHT)).clicked() {
                pending_side_changes.push((tab, "right"));
                ui.close();
            }
            if ui.button(format!("{} Bottom panel (dopesheet)", icons::DOCK_BOTTOM)).clicked() {
                pending_side_changes.push((tab, "bottom"));
                ui.close();
            }
            if ui.button(format!("{} Default side", icons::REVERSE)).clicked() {
                pending_side_changes.push((tab, "default"));
                ui.close();
            }
        });
    });
}

/// Single dispatch site for tab body rendering — used by both the dock panel
/// (when the active tab is docked) and each floating undocked window.
#[allow(clippy::too_many_arguments)]
fn render_tab_body(
    ui: &mut egui::Ui,
    tab: PoseControllerTab,
    pc: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    sender: Option<&PoseCommandSender>,
    active_anim: &mut ResMut<ActiveNativeAnimation>,
    streaming: &StreamingAnimation,
    kimodo_client: Option<&KimodoClientRes>,
    tokio_rt: Option<&SharedTokio>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    indexed: Option<&IndexedBones>,
    bone_influence: Option<&BoneInfluence>,
    material_vis: Option<&MaterialVisibilityStore>,
    rig_params: &mut super::rig_editor::RigTabSystemParam,
    settings: &mut Settings,
    intent_cal: Option<&SemanticIntentCalibrationHandle>,
    intent_wizard: Option<&IntentCalibrationWizardHandle>,
    undo: Option<&UndoHistory>,
    layer_ctx: &AnimationTabLayerContext<'_>,
    director: (
        Option<&mut crate::plugins::alive_director::DirectorState>,
        Option<&crate::plugins::alive_director::DirectorStatus>,
    ),
) {
    match tab {
        PoseControllerTab::Library => library_tab(ui, pc, library, sender, snapshot, undo),
        PoseControllerTab::Animation => animation_tab(
            ui,
            pc,
            library,
            streaming,
            active_anim,
            kimodo_client,
            tokio_rt,
            &mut settings.pose_controller,
            &mut settings.ui.show_anim_layers,
            layer_ctx,
            director,
        ),
        PoseControllerTab::Bones => bones_with_expressions_tab(
            ui,
            pc,
            sender,
            snapshot,
            indexed,
            bone_influence,
            material_vis,
            settings,
            rig_params,
            undo,
        ),
        PoseControllerTab::Rig => super::rig_editor::rig_tab(
            ui,
            pc,
            sender,
            indexed,
            snapshot,
            rig_params,
        ),
        PoseControllerTab::IntentLab => {
            intent_lab_tab(
                ui,
                pc,
                &*settings,
                sender,
                intent_cal,
                intent_wizard,
                snapshot,
                undo,
            )
        }
    }
}

/// Inline rig hover hint — adds widgets directly into the parent layout
/// (typically the menu bar). Surfaces the bone currently hovered / selected
/// and the active rotation axis so the user always knows which deform bone
/// an LMB / drag will affect, no matter which workspace tab is open.
pub(super) fn draw_rig_hover_hint(
    ui: &mut egui::Ui,
    pc: &mut PoseControllerUiState,
    rig: &crate::plugins::rig_editor::RigEditorState,
) {
    if let Some(h) = rig.hovered_axis {
        ui.label(egui::RichText::new(format!("(over {})", h.label())).weak());
    }
    let axis_color = match rig.active_axis {
        RigEditAxis::X => egui::Color32::from_rgb(235, 80, 80),
        RigEditAxis::Y => egui::Color32::from_rgb(90, 220, 100),
        RigEditAxis::Z => egui::Color32::from_rgb(110, 150, 240),
    };
    ui.colored_label(axis_color, rig.active_axis.label());

    if let Some(name) = rig.selected_bone.as_deref() {
        ui.label("selected:");
        let label = ui.colored_label(
            egui::Color32::from_rgb(200, 220, 255),
            egui::RichText::new(name).monospace().strong(),
        );
        if label.clicked() {
            pc.tab = PoseControllerTab::Rig;
        }
    } else {
        ui.label(egui::RichText::new("selected: —").weak());
    }

    ui.separator();
    if let Some(name) = rig.hovered_bone.as_deref() {
        ui.label("hover:");
        ui.colored_label(
            egui::Color32::from_rgb(220, 200, 120),
            egui::RichText::new(name).monospace(),
        );
    } else {
        ui.label(egui::RichText::new("hover: —").weak());
    }
    ui.separator();

    let mode = if rig.edit_mode { "edit" } else { "view" };
    let mode_color = if rig.edit_mode {
        egui::Color32::from_rgb(180, 220, 140)
    } else {
        egui::Color32::from_rgb(160, 160, 160)
    };
    ui.colored_label(mode_color, format!("[{mode}]"));
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

// ---------- Top transport toolbar ----------------------------------------------

/// Reset every bone to bind pose, clear slider cache, and record undo.
pub(super) fn reset_all_bones(
    state: &mut PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    undo: Option<&UndoHistory>,
) {
    if let (Some(h), Some(snap)) = (undo, snapshot) {
        h.record(snap, &state.bone_euler, "reset all bones");
    }
    state.bone_euler.clear();
    if let Some(s) = sender {
        s.send(PoseCommand::ResetPose);
    }
    state.status = Some("reset rig to bind pose".into());
}

/// Inline transport buttons (Reset / Stop native / Stop idle / Resume idle /
/// auto-stop checkbox). Adds widgets directly into the parent layout —
/// expected to be called from the menu bar so these stay reachable
/// regardless of which workspace tab is focused. The `play {name} {f}/{n}`
/// playback indicator is rendered separately via [`playback_indicator`] so
/// it can be placed in the menu bar's right-aligned section.
pub(super) fn transport_toolbar(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    active_anim: &mut ActiveNativeAnimation,
    commands: &mut Commands,
    vrma_entities: &[Entity],
    players_q: &mut Query<&mut AnimationPlayer>,
    pose_settings: &mut crate::config::PoseControllerSettings,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    undo: Option<&UndoHistory>,
) {
    if ui
        .add_enabled(!vrma_entities.is_empty(), egui::Button::new("▶"))
        .on_hover_text("Play the loaded idle VRMA on a loop again")
        .clicked()
    {
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
    ui.separator();
    if ui
        .button("⏹ native")
        .on_hover_text("Stop the native Bevy animation player")
        .clicked()
    {
        active_anim.stop();
        state.status = Some("stopped native animation".into());
    }
    ui.separator();
    if ui
        .button("⏹ idle")
        .on_hover_text("Stop every AnimationPlayer (idle VRMA sampler)")
        .clicked()
    {
        let mut n = 0usize;
        for mut player in players_q.iter_mut() {
            player.stop_all();
            n += 1;
        }
        state.status = Some(format!("stopped {n} AnimationPlayer(s)"));
    }
    ui.separator();
    if ui
        .button(icons::icon(icons::REFRESH))
        .on_hover_text("Reset pose + expressions (reset_pose MCP)")
        .clicked()
    {
        reset_all_bones(state, sender, snapshot, undo);
    }
    ui.separator();
    ui.checkbox(&mut pose_settings.auto_stop_idle_vrma, "auto-stop idle")
        .on_hover_text(
            "When on, any manual pose / expression command stops every VRMA so the writes stick.",
        );
}

/// `play {name} {frame}/{total}` indicator for the currently-playing native
/// animation. Renders nothing when no animation is active. Designed to be
/// added into the menu bar's right-aligned section so it sits next to the
/// pipeline status.
pub(super) fn playback_indicator(ui: &mut egui::Ui, active_anim: &ActiveNativeAnimation) {
    if let Some(name) = active_anim.current_name() {
        ui.label(
            egui::RichText::new(format!(
                "play {name}  {}/{}",
                active_anim.current_frame().unwrap_or(0),
                active_anim.frame_count(),
            ))
            .small()
            .color(egui::Color32::from_rgb(180, 220, 255)),
        );
    }
}

// ---------- Library (poses) tab -----------------------------------------------

/// Fixed width for pose list rows (edit + apply button); avoids full-panel stretch.
const POSE_LIBRARY_WIDTH: f32 = 280.0;

fn pose_library_width(ui: &egui::Ui) -> f32 {
    POSE_LIBRARY_WIDTH.min(ui.available_width().max(120.0))
}

fn library_tab(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    sender: Option<&PoseCommandSender>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    undo: Option<&UndoHistory>,
) {
    let categories: Vec<String> = collect_pose_categories(library);
    let content_w = pose_library_width(ui);
    ui.set_max_width(content_w);

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.search)
                .hint_text("filter…")
                .desired_width(92.0),
        );
        category_combobox(
            ui,
            "pose_filter_cat",
            &categories,
            &mut state.category_filter,
            "",
        );
        if ui
            .button(icons::icon(icons::REFRESH))
            .on_hover_text("Reload pose list from disk")
            .clicked()
        {
            library.mark_dirty();
        }
    });

    ui.collapsing("Snapshot current rig as new pose", |ui| {
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut state.snapshot_name).desired_width(180.0),
            );
        });
        ui.horizontal(|ui| {
            category_combobox(
                ui,
                "snapshot_cat_pick",
                &categories,
                &mut state.snapshot_category,
                "Category",
            );
        });
        let enabled = snapshot.is_some() && !state.snapshot_name.trim().is_empty();
        if ui
            .add_enabled(enabled, egui::Button::new("Save snapshot"))
            .on_hover_text("Capture the current rig pose into the library.")
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
                            crate::pose_library::BoneRotation {
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
                    expressions: snap.expressions.clone(),
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

    ui.separator();

    let poses = library.poses();
    let search = state.search.trim().to_ascii_lowercase();
    let cat = state.category_filter.trim().to_ascii_lowercase();

    // Group filtered poses by category so each renders in its own collapsible
    // section. BTreeMap keeps the section order stable/alphabetical.
    let mut grouped: std::collections::BTreeMap<String, Vec<PoseFile>> =
        std::collections::BTreeMap::new();
    for pose in poses {
        if !search.is_empty() && !pose.name.to_ascii_lowercase().contains(&search) {
            continue;
        }
        if !cat.is_empty() && pose.category.to_ascii_lowercase() != cat {
            continue;
        }
        let key = if pose.category.trim().is_empty() {
            "(uncategorized)".to_string()
        } else {
            pose.category.clone()
        };
        grouped.entry(key).or_default().push(pose);
    }

    egui::ScrollArea::vertical()
        .max_height(ui.available_height() / 1.1)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_max_width(content_w);
            if grouped.is_empty() {
                ui.label(egui::RichText::new("no poses match").weak());
                return;
            }
            for (category, poses) in &grouped {
                let header = format!("{} ({})", category, poses.len());
                egui::CollapsingHeader::new(header)
                    .id_salt(format!("pose-cat-section-{category}"))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.set_max_width(content_w);
                        for pose in poses {
                            pose_row(
                                ui, state, library, sender, &categories, pose, snapshot, undo,
                            );
                        }
                    });
            }
        });
}

/// Distinct pose categories, sorted, plus "(uncategorized)" for blank.
fn collect_pose_categories(library: &PoseLibraryAssets) -> Vec<String> {
    let mut out: Vec<String> = library
        .poses()
        .iter()
        .map(|p| {
            if p.category.trim().is_empty() {
                "(uncategorized)".into()
            } else {
                p.category.clone()
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Category combobox with an explicit "+ New category…" item that swaps the
/// combobox with a text input (gated by `state.new_category_buf`).
fn category_combobox(
    ui: &mut egui::Ui,
    id_salt: &str,
    categories: &[String],
    current: &mut String,
    label: &str,
) {
    if !label.is_empty() {
        ui.label(label);
    }
    let width = if label.is_empty() { 96.0 } else { 140.0 };
    let any_text = if label.is_empty() { "any" } else { "(any)" };
    egui::ComboBox::from_id_salt(id_salt)
        .width(width)
        .selected_text(if current.trim().is_empty() {
            any_text.into()
        } else {
            current.clone()
        })
        .show_ui(ui, |ui| {
            if ui.selectable_label(current.is_empty(), "(any)").clicked() {
                current.clear();
            }
            for c in categories {
                if ui.selectable_label(current == c, c).clicked() {
                    *current = c.clone();
                }
            }
        });
}

fn pose_row(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    sender: Option<&PoseCommandSender>,
    categories: &[String],
    pose: &PoseFile,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    undo: Option<&UndoHistory>,
) {
    let editing = state.editing_pose.as_deref() == Some(pose.name.as_str());

    if editing {
        pose_edit_row(ui, state, library, categories, pose);
        return;
    }

    let mut apply = false;
    ui.horizontal(|ui| {
        if ui
            .small_button(icons::icon(icons::EDIT))
            .on_hover_text("Edit (rename, change category, delete)")
            .clicked()
        {
            state.editing_pose = Some(pose.name.clone());
        }
        if ui
            .button(egui::RichText::new(&pose.name).strong())
            .on_hover_text("Apply this pose")
            .clicked()
        {
            apply = true;
        }
    });

    if apply {
        if let Some(s) = sender {
            if let (Some(h), Some(snap)) = (undo, snapshot) {
                h.record(snap, &state.bone_euler, format!("apply pose {}", pose.name));
            }
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
}

/// Inline edit row for a single pose: rename field, category picker, delete,
/// and done. Shown in place of the normal row while `state.editing_pose`
/// matches this pose.
fn pose_edit_row(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    categories: &[String],
    pose: &PoseFile,
) {
    let frame = egui::Frame::group(ui.style())
        .fill(egui::Color32::from_rgba_unmultiplied(80, 100, 160, 36));
    frame.show(ui, |ui| {
        ui.set_max_width(pose_library_width(ui));
        ui.horizontal(|ui| {
            ui.label("name");
            let buf = state
                .rename_buf
                .entry(pose.name.clone())
                .or_insert_with(|| pose.name.clone());
            let r = ui.add(egui::TextEdit::singleline(buf).desired_width(150.0));
            let commit = (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui
                    .button(icons::icon(icons::SAVE))
                    .on_hover_text("Rename (Enter)")
                    .clicked();
            if commit {
                let new_name = buf.trim().to_string();
                if !new_name.is_empty() && new_name != pose.name {
                    match library.library.rename_pose(&pose.name, &new_name) {
                        Ok(()) => {
                            state.status = Some(format!("{} → {new_name}", pose.name));
                            library.mark_dirty();
                            state.editing_pose = Some(new_name);
                        }
                        Err(e) => state.status = Some(format!("rename failed: {e}")),
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("category");
            if state.new_category_buf.contains_key(&pose.name) {
                let buf = state.new_category_buf.get_mut(&pose.name).unwrap();
                let r = ui.add(egui::TextEdit::singleline(buf).desired_width(110.0));
                if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    || ui.button(icons::icon(icons::SAVE)).clicked()
                {
                    commit_new_category(library, state, &pose.name);
                }
                if ui.button(icons::icon(icons::CLOSE)).clicked() {
                    state.new_category_buf.remove(&pose.name);
                }
            } else {
                let cat_buf = state
                    .category_buf
                    .entry(pose.name.clone())
                    .or_insert_with(|| pose.category.clone());
                egui::ComboBox::from_id_salt(format!("pose-cat-{}", pose.name))
                    .width(120.0)
                    .selected_text(cat_buf.clone())
                    .show_ui(ui, |ui| {
                        for c in categories {
                            if ui.selectable_label(cat_buf == c, c).clicked() {
                                *cat_buf = c.clone();
                                let _ = library.library.update_pose_category(&pose.name, c);
                                library.mark_dirty();
                                state.status = Some(format!("{} category → {c}", pose.name));
                            }
                        }
                        ui.separator();
                        if ui.selectable_label(false, "+ New category…").clicked() {
                            state
                                .new_category_buf
                                .insert(pose.name.clone(), String::new());
                        }
                    });
            }
        });
        ui.horizontal(|ui| {
            if ui
                .button(icons::menu_item(icons::TRASH, "Delete"))
                .on_hover_text("Delete this pose from the library")
                .clicked()
            {
                match library.library.delete_pose(&pose.name) {
                    Ok(()) => {
                        state.status = Some(format!("deleted {}", pose.name));
                        library.mark_dirty();
                        state.editing_pose = None;
                    }
                    Err(e) => state.status = Some(format!("delete failed: {e}")),
                }
            }
            if ui
                .button(icons::menu_item(icons::STATUS_READY, "Done"))
                .clicked()
            {
                state.editing_pose = None;
            }
        });
    });
}

/// Commit "+ New category" inline-text input → write to disk + refresh.
fn commit_new_category(
    library: &PoseLibraryAssets,
    state: &mut PoseControllerUiState,
    pose_name: &str,
) {
    let Some(buf) = state.new_category_buf.remove(pose_name) else {
        return;
    };
    let new_cat = buf.trim().to_string();
    if new_cat.is_empty() {
        return;
    }
    if let Err(e) = library.library.update_pose_category(pose_name, &new_cat) {
        state.status = Some(format!("category failed: {e}"));
        return;
    }
    state
        .category_buf
        .insert(pose_name.to_string(), new_cat.clone());
    library.mark_dirty();
    state.status = Some(format!("{pose_name} category → {new_cat}"));
}

// ---------- Animation workspace (merged Animations + AI Gen + Idle) -----------

/// New Animation workspace.
///
/// Layout: top filter strip + category combobox; left = animation list grouped
/// by category; right = generator + idle controls. Replaces the three old
/// sibling tabs (`Animations`, `AiGen`, `Idle`).
fn animation_tab(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    streaming: &StreamingAnimation,
    active_anim: &mut ResMut<ActiveNativeAnimation>,
    kimodo: Option<&KimodoClientRes>,
    tokio_rt: Option<&SharedTokio>,
    pose_settings: &mut crate::config::PoseControllerSettings,
    show_anim_layers: &mut bool,
    layer_ctx: &AnimationTabLayerContext<'_>,
    director: (
        Option<&mut crate::plugins::alive_director::DirectorState>,
        Option<&crate::plugins::alive_director::DirectorStatus>,
    ),
) {
    let cats = collect_animation_categories(library);

    ui.horizontal(|ui| {
        ui.label("Filter");
        ui.add(egui::TextEdit::singleline(&mut state.search).desired_width(140.0));
        category_combobox(ui, "anim_filter_cat", &cats, &mut state.category_filter, "Category");
        ui.separator();
        ui.menu_button(format!("{:?} {}", state.default_playback_mode, icons::CHEV_OPEN), |ui| {
            ui.selectable_value(
                &mut state.default_playback_mode,
                PlaybackMode::Native,
                "Native",
            );
            ui.selectable_value(
                &mut state.default_playback_mode,
                PlaybackMode::Kimodo, "Kimodo",
            );
        });
        if ui.button("⟲").clicked() {
            library.mark_dirty();
        }
    });

    ui.separator();

    // Right: generator + idle controls.
    ui.vertical(|ui| {
        egui::CollapsingHeader::new("Generator").default_open(true).show(ui, |ui| {
            ai_gen_panel(ui, state, library, streaming, kimodo, tokio_rt);
        });
        egui::CollapsingHeader::new("Idle").show(ui, |ui| {
            idle_panel(ui, pose_settings);
        });
        egui::CollapsingHeader::new("Director").show(ui, |ui| {
            director_panel(ui, pose_settings, director);
        });
    });

    ui.vertical_centered(|ui| {
        ui.add_space(10.);
        ui.label(egui::RichText::new("Library").strong());
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt("anim_workspace_scroll")
        .show(ui, |ui| {

            let anims = library.animations();
            let search = state.search.trim().to_ascii_lowercase();
            let cat = state.category_filter.trim().to_ascii_lowercase();
            let mut grouped: BTreeMap<String, Vec<AnimationMeta>> = BTreeMap::new();
            for meta in anims {
                if !search.is_empty()
                    && !meta.name.to_ascii_lowercase().contains(&search)
                {
                    continue;
                }
                if !cat.is_empty() && meta.category.to_ascii_lowercase() != cat {
                    continue;
                }
                let key = if meta.category.trim().is_empty() {
                    "(uncategorized)".to_string()
                } else {
                    meta.category.clone()
                };
                grouped.entry(key).or_default().push(meta);
            }
            if grouped.is_empty() {
                ui.label(egui::RichText::new("(no animations match)").italics());
            }
            for (cat_key, metas) in &grouped {
                egui::CollapsingHeader::new(format!("{cat_key} ({})", metas.len()))
                    .id_salt(format!("anim-cat-{cat_key}"))
                    .default_open(false)
                    .show(ui, |ui| {
                        for meta in metas {
                            anim_row(
                                ui,
                                state,
                                library,
                                active_anim,
                                kimodo,
                                meta,
                                show_anim_layers,
                                layer_ctx,
                            );
                        }
                    });
            }
        });
}

fn collect_animation_categories(library: &PoseLibraryAssets) -> Vec<String> {
    let mut out: Vec<String> = library
        .animations()
        .iter()
        .map(|m| {
            if m.category.trim().is_empty() {
                "(uncategorized)".into()
            } else {
                m.category.clone()
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn cancel_animation_layer_edit(
    session: &AnimationLayerEditSession,
    state: &mut PoseControllerUiState,
    layer_ctx: &AnimationTabLayerContext<'_>,
) {
    if let Some(stack) = layer_ctx.stack {
        stack.with_write(|s| *s = session.stack_backup.clone());
    }
    state.editing_animation = None;
    state.animation_layer_edit = None;
    state.status = Some(format!(
        "cancelled layer edit for {} (stack restored)",
        session.filename
    ));
}

fn commit_animation_layer_edit(
    filename: &str,
    meta: &AnimationMeta,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    layer_ctx: &AnimationTabLayerContext<'_>,
) {
    let Some(stack) = layer_ctx.stack else {
        state.status = Some("layer stack unavailable".into());
        return;
    };
    let Some(rest) = layer_ctx.rest else {
        state.status = Some("rest pose not ready — wait for VRM to load".into());
        return;
    };
    if rest.captured == 0 {
        state.status = Some("rest pose not ready — wait for VRM to load".into());
        return;
    }
    let fallback_dur = meta.frame_count as f32 / meta.fps.max(1.0) as f32;
    let baked = stack.with_read(|s| {
        bake_layer_stack_to_animation(
            s,
            rest,
            layer_ctx.hierarchy,
            meta.fps,
            Some(fallback_dur),
        )
    });
    let cat_buf = state
        .anim_category_buf
        .get(filename)
        .cloned()
        .unwrap_or_else(|| meta.category.clone());
    let hold = state
        .anim_hold_buf
        .get(filename)
        .copied()
        .unwrap_or(meta.hold_duration);
    let looping = state
        .anim_loop_buf
        .get(filename)
        .copied()
        .unwrap_or(meta.looping);
    let mut out = baked;
    out.name = meta.name.clone();
    out.category = {
        let c = cat_buf.trim();
        if c.is_empty() {
            None
        } else {
            Some(c.to_string())
        }
    };
    out.looping = Some(looping);
    out.hold_duration = Some(hold);

    match library.library.write_animation_at(filename, &out) {
        Err(e) => {
            state.status = Some(format!("save animation failed: {e}"));
            return;
        }
        Ok(path) => {
            if let Some(store) = layer_ctx.layer_sets {
                let set_name = animation_edit_layer_set_name(filename);
                stack.with_read(|s| store.save_current(&set_name, s));
                store.persist();
                state.status = Some(format!(
                    "saved {} + layer set '{set_name}' ({})",
                    path.display(),
                    out.frame_count
                ));
            } else {
                state.status = Some(format!(
                    "saved {} ({} frames; layer set store unavailable)",
                    path.display(),
                    out.frame_count
                ));
            }
            library.mark_dirty();
        }
    }
}

fn anim_row(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    active_anim: &mut ResMut<ActiveNativeAnimation>,
    kimodo: Option<&KimodoClientRes>,
    meta: &AnimationMeta,
    show_anim_layers: &mut bool,
    layer_ctx: &AnimationTabLayerContext<'_>,
) {
    let editing = state.editing_animation.as_deref() == Some(meta.filename.as_str());
    let renaming = state.renaming_animation.as_deref() == Some(meta.filename.as_str());
    let frame_color = if editing {
        egui::Color32::from_rgba_unmultiplied(80, 100, 160, 36)
    } else {
        ui.style().visuals.faint_bg_color
    };
    let frame = egui::Frame::group(ui.style()).fill(frame_color);
    let response = frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            // Title cell — click to rename.
            if renaming {
                let buf = state
                    .anim_rename_buf
                    .entry(meta.filename.clone())
                    .or_insert_with(|| meta.filename.clone());
                ui.add(egui::TextEdit::singleline(buf).desired_width(180.0));
                if ui.button("save").clicked() {
                    let new_name = buf.trim().to_string();
                    if !new_name.is_empty() && new_name != meta.filename {
                        let new_name = if new_name.ends_with(".json") {
                            new_name
                        } else {
                            format!("{new_name}.json")
                        };
                        if library
                            .library
                            .rename_animation(&meta.filename, &new_name)
                            .is_ok()
                        {
                            library.mark_dirty();
                        }
                    }
                    state.renaming_animation = None;
                }
                if ui.button("cancel").clicked() {
                    state.renaming_animation = None;
                }
            } else {
                let title_resp = ui.button(egui::RichText::new(&meta.name).strong())
                    .on_hover_text("Click to rename. Use the play buttons to start playback.");
                if title_resp.clicked() {
                    state.renaming_animation = Some(meta.filename.clone());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("▶")
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
                    .button("▶ (K)")
                    .on_hover_text("Ask Kimodo peer to stream it")
                    .clicked()
                {
                    if let Some(k) = kimodo {
                        k.play_saved_animation(meta.filename.clone());
                        state.status = Some(format!("kimodo play {}", meta.name));
                    }
                }
                if editing {
                    if ui
                        .button("done")
                        .on_hover_text(
                            "Bake layer stack → overwrite library clip + save layer-set preset",
                        )
                        .clicked()
                    {
                        commit_animation_layer_edit(
                            &meta.filename,
                            meta,
                            state,
                            library,
                            layer_ctx,
                        );
                        state.editing_animation = None;
                        state.animation_layer_edit = None;
                    }
                    if ui
                        .button("cancel")
                        .on_hover_text("Discard layer-stack edits and restore the previous stack")
                        .clicked()
                    {
                        if let Some(session) = state.animation_layer_edit.clone() {
                            cancel_animation_layer_edit(&session, state, layer_ctx);
                        }
                    }
                } else if ui
                    .button(icons::icon(icons::EDIT))
                    .on_hover_text(
                        "Edit via animation layer stack — one layer per bone (opens Layers panel; Done bakes + saves)",
                    )
                    .clicked()
                {
                    active_anim.stop();
                    *show_anim_layers = true;
                    let set_name = animation_edit_layer_set_name(&meta.filename);
                    let stack_backup = layer_ctx
                        .stack
                        .map(|h| h.with_read(|s| s.clone()))
                        .unwrap_or_default();
                    if let (Some(stack), Some(store)) = (layer_ctx.stack, layer_ctx.layer_sets) {
                        match begin_library_animation_edit(
                            &meta.filename,
                            &library.library,
                            store,
                            stack,
                        ) {
                            Ok(msg) => state.status = Some(msg),
                            Err(e) => state.status = Some(format!("layer edit failed: {e}")),
                        }
                    } else {
                        state.status =
                            Some("layer stack / layer sets unavailable".into());
                    }
                    state.editing_animation = Some(meta.filename.clone());
                    state.animation_layer_edit = Some(AnimationLayerEditSession {
                        filename: meta.filename.clone(),
                        layer_set_name: set_name,
                        stack_backup,
                    });
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(format!(
                        "{}fr @{:.0}fps",
                        meta.frame_count, meta.fps
                    ))
                    .weak(),
                );
            });
        });
        if editing {
            let layer_editing = state
                .animation_layer_edit
                .as_ref()
                .is_some_and(|s| s.filename == meta.filename);
            if layer_editing {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(160, 200, 255),
                        "Layer-stack edit (one layer per bone) — use Animation Layers panel, then Done to bake",
                    );
                    if ui.button("Layers").clicked() {
                        *show_anim_layers = true;
                    }
                    if ui.button("Cancel edit").clicked() {
                        if let Some(session) = state.animation_layer_edit.clone() {
                            cancel_animation_layer_edit(&session, state, layer_ctx);
                        }
                    }
                });
            }
            ui.horizontal(|ui| {
                let cat_buf = state
                    .anim_category_buf
                    .entry(meta.filename.clone())
                    .or_insert_with(|| meta.category.clone());
                ui.label("Category");
                ui.add(egui::TextEdit::singleline(cat_buf).desired_width(110.0));
                let looping = state
                    .anim_loop_buf
                    .entry(meta.filename.clone())
                    .or_insert(meta.looping);
                ui.checkbox(looping, "Looping");
                let hold_buf = state
                    .anim_hold_buf
                    .entry(meta.filename.clone())
                    .or_insert(meta.hold_duration);
                ui.label("Hold (s)");
                ui.add(egui::Slider::new(hold_buf, 0.0..=10.0).step_by(0.1));
                if ui.button("Save").clicked() {
                    let new_cat = cat_buf.trim().to_string();
                    let new_hold = *hold_buf;
                    let new_loop = *looping;
                    let _ = library.library.update_animation_metadata(
                        &meta.filename,
                        if new_cat.is_empty() {
                            None
                        } else {
                            Some(new_cat)
                        },
                        Some(new_loop),
                        Some(new_hold),
                    );
                    library.mark_dirty();
                    state.status = Some(format!("metadata saved for {}", meta.filename));
                }
                if ui.button("Delete").on_hover_text("Delete this animation").clicked() {
                    if library.library.delete_animation(&meta.filename).is_ok() {
                        state.status = Some(format!("deleted {}", meta.filename));
                        library.mark_dirty();
                        state.editing_animation = None;
                        state.animation_layer_edit = None;
                    }
                }
            });
        }
    });
    let _ = response;
}

// ---------- AI Gen + Idle inline panels ----------------------------------------

fn ai_gen_panel(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    library: &PoseLibraryAssets,
    streaming: &StreamingAnimation,
    kimodo: Option<&KimodoClientRes>,
    tokio_rt: Option<&SharedTokio>,
) {
    ui.label(egui::RichText::new("AI generation (Kimodo)").strong());
    ui.add(
        egui::TextEdit::multiline(&mut state.gen_prompt)
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .hint_text("describe the motion…"),
    );
    ui.horizontal(|ui| {
        ui.label("Duration (s)");
        ui.add(egui::Slider::new(&mut state.gen_duration, 0.5..=20.0).step_by(0.1));
    });
    ui.horizontal(|ui| {
        ui.label("Steps");
        ui.add(egui::Slider::new(&mut state.gen_steps, 10..=500));
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut state.gen_stream, "Stream frames");
        ui.add(
            egui::TextEdit::singleline(&mut state.gen_save_name)
                .hint_text("save as (optional)")
                .desired_width(130.0),
        );
    });
    ui.horizontal(|ui| {
        let enabled = kimodo.is_some() && tokio_rt.is_some() && !state.gen_prompt.trim().is_empty();
        if ui
            .add_enabled(enabled, egui::Button::new(format!("{} Generate", icons::MAGIC)))
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
                    ..Default::default()
                };
                let client = (*k).clone();
                state.status = Some("generate queued".into());
                rt.spawn(async move {
                    match client.generate_motion(req).await {
                        Ok(out) => info!(
                            "kimodo generate finished: {} ({})",
                            out.final_status, out.final_message
                        ),
                        Err(e) => warn!("kimodo generate failed: {e}"),
                    }
                });
                library.mark_dirty();
            }
        }
    });
    ui.small(format!(
        "Streaming: active={} pending={}",
        streaming.active_request_id().is_some(),
        streaming.pending_frames()
    ));
}

fn idle_panel(
    ui: &mut egui::Ui,
    settings: &mut crate::config::PoseControllerSettings,
) {
    ui.label(egui::RichText::new("Idle loop").strong());
    ui.checkbox(&mut settings.idle_enabled, "Enable local idle loop");
    ui.horizontal(|ui| {
        ui.label("min");
        ui.add(egui::Slider::new(
            &mut settings.idle_interval_min_sec,
            1.0..=120.0,
        ));
    });
    ui.horizontal(|ui| {
        ui.label("max");
        ui.add(egui::Slider::new(
            &mut settings.idle_interval_max_sec,
            1.0..=300.0,
        ));
    });
    ui.horizontal(|ui| {
        ui.label("category filter");
        ui.add(
            egui::TextEdit::singleline(&mut settings.idle_category).desired_width(120.0),
        );
    });
    ui.separator();
    ui.checkbox(
        &mut settings.blend_transitions_enabled,
        "Honor blend / transition",
    );
    ui.horizontal(|ui| {
        ui.label("transition (s)");
        ui.add(
            egui::Slider::new(&mut settings.default_transition_seconds, 0.0..=5.0).step_by(0.05),
        );
    });
    ui.horizontal(|ui| {
        ui.label("weight");
        ui.add(egui::Slider::new(&mut settings.default_blend_weight, 0.0..=1.0).step_by(0.05));
    });
}

fn director_panel(
    ui: &mut egui::Ui,
    settings: &mut crate::config::PoseControllerSettings,
    director: (
        Option<&mut crate::plugins::alive_director::DirectorState>,
        Option<&crate::plugins::alive_director::DirectorStatus>,
    ),
) {
    let (state, status) = director;
    ui.horizontal(|ui| {
        ui.checkbox(&mut settings.director_enabled, "Enabled")
            .on_hover_text("Presence-aware autonomous idle episodes (poses, gestures, gaze).");
        if let Some(st) = status {
            let flags = format!(
                "{}{}",
                if st.attentive { " · attentive" } else { "" },
                if st.speaking { " · speaking" } else { "" },
            );
            ui.label(
                egui::RichText::new(format!(
                    "next {:.0}s · {}{}",
                    st.next_pick_in, st.last_episode, flags
                ))
                .small()
                .color(theme::weak_text(ui)),
            );
        }
        if let Some(state) = state {
            if ui
                .small_button("Fire now")
                .on_hover_text("Trigger the next episode immediately")
                .clicked()
            {
                state.next_pick_in = Some(std::time::Duration::ZERO);
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("expressive p");
        ui.add(egui::Slider::new(&mut settings.director_expressive_prob, 0.0..=0.9).step_by(0.05));
        ui.label("gaze p");
        ui.add(egui::Slider::new(&mut settings.director_gaze_prob, 0.0..=0.9).step_by(0.05));
    });
    ui.horizontal(|ui| {
        ui.label("boost ramp (s)");
        ui.add(egui::Slider::new(&mut settings.director_boost_ramp_secs, 0.1..=3.0).step_by(0.1));
        ui.checkbox(&mut settings.director_micro_expressions, "micro-expr")
            .on_hover_text("Faint auto-retiring face flickers on expressive beats.");
    });
    ui.horizontal(|ui| {
        ui.label("finger boost ×");
        ui.add(egui::Slider::new(&mut settings.director_finger_boost_mul, 1.0..=4.0).step_by(0.1));
        ui.label("toe ×");
        ui.add(egui::Slider::new(&mut settings.director_toe_boost_mul, 1.0..=4.0).step_by(0.1));
    });
}

// ---------- Expressions panel (right side of Bones workspace) -----------------

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

fn expressions_panel(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    pose_settings: &crate::config::PoseControllerSettings,
) {
    ui.label(egui::RichText::new("VRM expression presets").strong())
        .on_hover_text(format!(
            "Idle VRMA can overwrite morphs unless auto-stop idle is on in the toolbar (currently {}).",
            if pose_settings.auto_stop_idle_vrma {
                "on"
            } else {
                "off"
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
    let scroll_h = ui.available_height().max(48.0);
    egui::ScrollArea::vertical()
        .id_salt("pose_vrm_expression_presets")
        .max_height(scroll_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
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

// ---------- Bones + Expressions split workspace -------------------------------

/// New Bones workspace — the bone list (left) + Expressions sliders (right).
/// Diagnostics are no longer in-line; they live in a collapsible footer to
/// keep the workspace focused on posing.
fn bones_with_expressions_tab(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    indexed: Option<&IndexedBones>,
    bone_influence: Option<&BoneInfluence>,
    material_vis: Option<&MaterialVisibilityStore>,
    settings: &mut Settings,
    rig_params: &mut super::rig_editor::RigTabSystemParam,
    undo: Option<&UndoHistory>,
) {
    let super::rig_editor::RigTabSystemParam {
        rig,
        mirror,
        vrm_q,
        springs,
        colliders,
    } = rig_params;
    let rig: &mut crate::plugins::rig_editor::RigEditorState = rig;
    let mirror: &mut crate::plugins::mirror::MirrorState = mirror;

    let avail_h = ui.available_height();
    let total_w = ui.available_width();
    let left_w = (total_w * 0.62).max(280.0);
    let right_w = (total_w - left_w - 12.0).max(160.0);

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(left_w, avail_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                bones_panel(
                    ui, state, sender, snapshot, indexed, bone_influence, material_vis, rig,
                    mirror, undo,
                );
            },
        );
        ui.separator();
        // Right column: expression presets (top half) stacked over the spring
        // bone physics panels (bottom half), each its own vertical scroll list.
        ui.allocate_ui_with_layout(
            egui::vec2(right_w, avail_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                let half = ((avail_h - 8.0) * 0.5).max(120.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(right_w, half),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let clip = ui.max_rect();
                        ui.set_clip_rect(clip);
                        ui.set_max_size(egui::vec2(right_w, half));
                        expressions_panel(
                            ui,
                            state,
                            sender,
                            snapshot,
                            &settings.pose_controller,
                        );
                    },
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(right_w, half),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let clip = ui.max_rect();
                        ui.set_clip_rect(clip);
                        ui.set_max_size(egui::vec2(right_w, half));
                        ui.label(egui::RichText::new("Spring bone physics").strong());
                        let scroll_h = ui.available_height().max(48.0);
                        egui::ScrollArea::vertical()
                            .id_salt("pose_spring_bone_physics")
                            .max_height(scroll_h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                super::rig_editor::spring_panels(
                                    ui, settings, rig, vrm_q, springs, colliders,
                                );
                            });
                    },
                );
            },
        );
    });
}

/// Manual bone controls — same diagnostic / sliders / list-grouping the old
/// Bones tab had, minus the heavy explanatory text (which now lives in a
/// collapsible footer). `bones_with_expressions_tab` wraps this in a split
/// layout against [`expressions_panel`].
fn bones_panel(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    indexed: Option<&IndexedBones>,
    bone_influence: Option<&BoneInfluence>,
    material_vis: Option<&MaterialVisibilityStore>,
    rig: &mut crate::plugins::rig_editor::RigEditorState,
    mirror: &mut crate::plugins::mirror::MirrorState,
    undo: Option<&UndoHistory>,
) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.bone_search)
                .hint_text("Bone filter")
                .desired_width((ui.available_width() - 90.0).clamp(120.0, 240.0)),
        );
        if ui.button(icons::icon(icons::CLOSE)).clicked() {
            state.bone_search.clear();
        }
    });
    let filter_lc = state.bone_search.trim().to_ascii_lowercase();

    ui.horizontal(|ui| {
        if ui
            .button(icons::menu_item(icons::REFRESH, "Reset all"))
            .on_hover_text("Reset every bone back to bind (rest) pose.")
            .clicked()
        {
            reset_all_bones(state, sender, snapshot, undo);
        }
        if ui
            .button(format!("{} Snapshot {} sliders", icons::CAMERA, icons::ARROW_RIGHT))
            .on_hover_text(
                "Seed the sliders with the current rig rotations so you can nudge from live pose.",
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
                    if yaw_extra != 0.0 && deg.iter().any(|v| v.abs() > 0.5) {
                        deg[1] = wrap_deg_180_signed(deg[1] - yaw_extra);
                    }
                    state.bone_euler.insert(name.clone(), deg);
                }
                state.status = Some(format!(
                    "seeded {} bone slider(s)",
                    snap.bones.len()
                ));
            } else {
                state.status = Some("no snapshot yet".into());
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("tracked {}", state.bone_euler.len())).weak(),
            );
        });
    });

    // Declutter toggles for the extra-bone list (need the per-model sidecar).
    if bone_influence.is_some() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut state.bones_show_non_deforming, "Show non-deforming")
                .on_hover_text(
                    "Reveal bones that move no geometry (toe _03 tips, hair/ribbon tips). \
Hidden by default to declutter the extra-bone list.",
                );
            ui.checkbox(&mut state.bones_hide_hidden_mesh, "Hide hidden-mesh")
                .on_hover_text(
                    "Hide bones whose every influenced mesh is currently hidden \
(e.g. waistBelt bones once the belt material is hidden in Graphics).",
                );
        });
    }

    ui.separator();

    // Consume the one-shot scroll target from the Rig tab (or viewport pick).
    let scroll_target = rig.pending_scroll_to_bone.take();

    egui::ScrollArea::vertical()
        .id_salt("bones_panel_scroll")
        .max_height(ui.available_height().max(200.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let grouped: HashSet<&str> = BONE_GROUPS
                .iter()
                .flat_map(|(_, b)| b.iter().copied())
                .collect();

            let st = scroll_target.as_deref();

            for (group_name, bones) in BONE_GROUPS {
                let filtered: Vec<&str> = bones
                    .iter()
                    .copied()
                    .filter(|b| bone_name_matches_search(&filter_lc, b))
                    .collect();
                if !filter_lc.is_empty() && filtered.is_empty() {
                    continue;
                }
                let force_open = st.is_some_and(|t| filtered.iter().any(|b| *b == t));
                egui::CollapsingHeader::new(format!("{group_name} ({})", filtered.len()))
                    .id_salt(format!("bones-group-{group_name}"))
                    .default_open(false)
                    .open(if force_open { Some(true) } else { None })
                    .show(ui, |ui| {
                        for bone in filtered {
                            bone_row(ui, state, sender, indexed, rig, mirror, bone, st, snapshot, undo);
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
                let force_open = st.is_some_and(|t| other_humanoid.iter().any(|b| *b == t));
                egui::CollapsingHeader::new(format!(
                    "Other · standard humanoid ({})",
                    other_humanoid.len()
                ))
                .id_salt("bones-other-humanoid")
                .default_open(false)
                .open(if force_open { Some(true) } else { None })
                .show(ui, |ui| {
                    for bone in other_humanoid {
                        bone_row(ui, state, sender, indexed, rig, mirror, bone, st, snapshot, undo);
                    }
                });
            }

            if let Some(idx) = indexed {
                let show_nondef = state.bones_show_non_deforming;
                let hide_hidden_mesh = state.bones_hide_hidden_mesh;
                // Declutter only when no bone filter is typed, so search always
                // reaches every bone regardless of the toggles.
                let declutter = filter_lc.is_empty();
                let mut def_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
                let mut extra_groups: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
                let mut hidden_nondef = 0usize;
                let mut hidden_mesh = 0usize;
                for n in &idx.names {
                    if is_vrm_humanoid_bone(n.as_str()) {
                        continue;
                    }
                    if !bone_name_matches_search(&filter_lc, n.as_str()) {
                        continue;
                    }
                    if declutter {
                        if let Some(bi) = bone_influence {
                            if !show_nondef && bi.is_non_deforming(n.as_str()) {
                                hidden_nondef += 1;
                                continue;
                            }
                            if hide_hidden_mesh
                                && bi.all_meshes_hidden(n.as_str(), |m| {
                                    material_vis.map(|mv| !mv.is_visible(m)).unwrap_or(false)
                                })
                            {
                                hidden_mesh += 1;
                                continue;
                            }
                        }
                    }
                    if let Some(cat) = def_bone_category_key(n.as_str()) {
                        def_groups.entry(cat).or_default().push(n.clone());
                    } else {
                        extra_groups
                            .entry(extra_bone_category(n.as_str()))
                            .or_default()
                            .push(n.clone());
                    }
                }
                let cmp_ci =
                    |a: &String, b: &String| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase());
                for bones in def_groups.values_mut() {
                    bones.sort_by(cmp_ci);
                }
                for bones in extra_groups.values_mut() {
                    bones.sort_by(cmp_ci);
                }

                for (cat_key, bones) in &def_groups {
                    if bones.is_empty() {
                        continue;
                    }
                    let title = format!(
                        "DEF · {} ({})",
                        format_def_category_title(cat_key),
                        bones.len()
                    );
                    let force_open = st.is_some_and(|t| bones.iter().any(|b| b.as_str() == t));
                    egui::CollapsingHeader::new(title)
                        .id_salt(format!("bones-def-{cat_key}"))
                        .default_open(false)
                        .open(if force_open { Some(true) } else { None })
                        .show(ui, |ui| {
                            for bone in bones {
                                bone_row(ui, state, sender, indexed, rig, mirror, bone.as_str(), st, snapshot, undo);
                            }
                        });
                }

                for (cat, bones) in &extra_groups {
                    if bones.is_empty() {
                        continue;
                    }
                    let title = format!("{cat} ({})", bones.len());
                    let force_open = st.is_some_and(|t| bones.iter().any(|b| b.as_str() == t));
                    egui::CollapsingHeader::new(title)
                        .id_salt(format!("bones-extra-{cat}"))
                        .default_open(false)
                        .open(if force_open { Some(true) } else { None })
                        .show(ui, |ui| {
                            for bone in bones {
                                bone_row(ui, state, sender, indexed, rig, mirror, bone.as_str(), st, snapshot, undo);
                            }
                        });
                }

                if hidden_nondef > 0 || hidden_mesh > 0 {
                    let mut parts: Vec<String> = Vec::new();
                    if hidden_nondef > 0 {
                        parts.push(format!("{hidden_nondef} non-deforming"));
                    }
                    if hidden_mesh > 0 {
                        parts.push(format!("{hidden_mesh} hidden-mesh"));
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} bone(s) hidden — toggles above",
                            parts.join(" + ")
                        ))
                        .weak()
                        .small(),
                    );
                }
            }
        });
}

/// One calibration-sign row: `[slider value] [Flip] label`. The Flip button
/// sits right after the slider's drag value so the controls line up vertically
/// across rows, with the descriptive label trailing.
fn cal_sign_row(ui: &mut egui::Ui, value: &mut f32, label: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = 150.0;
        ui.add(egui::Slider::new(value, -1.0..=1.0).step_by(0.01));
        if ui
            .button(icons::menu_item(icons::REVERSE, "Flip"))
            .on_hover_text(format!("Flip sign of {label}"))
            .clicked()
        {
            *value *= -1.0;
        }
        ui.label(label);
    });
}

fn intent_lab_tab(
    ui: &mut egui::Ui,
    pc: &mut PoseControllerUiState,
    settings: &Settings,
    sender: Option<&PoseCommandSender>,
    cal_handle: Option<&SemanticIntentCalibrationHandle>,
    wizard_handle: Option<&IntentCalibrationWizardHandle>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    undo: Option<&UndoHistory>,
) {
    ui.label(egui::RichText::new("Intent Lab — semantic MCP tools").strong());
    ui.label(format!(
        "Tune sign multipliers per loaded VRM so AI-facing tools (raise_leg, bend_knee, arms_down_rest) \
move the body the way you expect. Use the AI wizard for a guided probe {arrow} confirm loop.",
        arrow = icons::ARROW_RIGHT
    ));

    let Some(cal_h) = cal_handle else {
        ui.colored_label(
            theme::warn(ui),
            "IntentCalibrationPlugin not loaded — add it before McpPlugin in main.rs.",
        );
        return;
    };

    let key = vrm_preset_key(settings.avatar.model_path.as_str());
    if pc.intent_lab_sync_key != key {
        pc.intent_lab_sync_key = key.clone();
        pc.intent_lab_cal = cal_h.0.read().unwrap().get(&key);
    }

    ui.monospace(format!("model_path: {}", settings.avatar.model_path));
    ui.monospace(format!("semantic_vrm_key: {key}"));

    ui.separator();
    ui.label(egui::RichText::new("AI calibration wizard").strong());
    ui.label(
        "Ask the agent to call begin_intent_calibration_wizard, then intent_calibration_probe \
each step. While awaiting confirm, answer in chat or use the buttons below.",
    );
    if let Some(wiz_h) = wizard_handle {
        let mut wiz = wiz_h.0.write().unwrap();
        match &wiz.phase {
            WizardPhase::Idle => {
                ui.label(egui::RichText::new("No active wizard session.").weak());
            }
            WizardPhase::Active {
                draft,
                step_index,
                awaiting,
                log,
                ..
            } => {
                pc.intent_lab_cal = draft.clone();
                ui.label(format!(
                    "Active — step {} / {}",
                    step_index + 1,
                    WIZARD_STEPS.len()
                ));
                if log.is_empty() {
                    ui.label("Completed steps: none yet");
                } else {
                    ui.label(format!("Completed steps: {}", log.len()));
                }
                if let Some(pending) = awaiting.clone() {
                    ui.colored_label(
                        theme::warn(ui),
                        "AWAITING YOUR CONFIRM",
                    );
                    ui.label(egui::RichText::new(&pending.label).strong());
                    ui.label(&pending.user_question);
                    let step_id = pending.step_id.clone();
                    ui.horizontal(|ui| {
                        if ui.button("Correct").clicked() {
                            match wiz.confirm(&step_id, ConfirmVerdict::Correct) {
                                Ok(_) => pc.status = Some(format!("wizard: {step_id} {} correct", icons::ARROW_RIGHT)),
                                Err(e) => pc.status = Some(format!("wizard confirm: {e}")),
                            }
                        }
                        if ui.button("Flip sign").clicked() {
                            match wiz.confirm(&step_id, ConfirmVerdict::Flip) {
                                Ok(_) => pc.status = Some(format!("wizard: {step_id} {} flip", icons::ARROW_RIGHT)),
                                Err(e) => pc.status = Some(format!("wizard confirm: {e}")),
                            }
                        }
                        if ui.button("Skip").clicked() {
                            match wiz.confirm(&step_id, ConfirmVerdict::Skip) {
                                Ok(_) => pc.status = Some(format!("wizard: {step_id} {} skip", icons::ARROW_RIGHT)),
                                Err(e) => pc.status = Some(format!("wizard confirm: {e}")),
                            }
                        }
                    });
                    if let Some(d) = wiz.draft_if_active() {
                        pc.intent_lab_cal = d.clone();
                    }
                } else if ui.button("Apply probe locally (same as MCP)").clicked() {
                    match wiz.probe_bones() {
                        Ok((bones, pending)) => {
                            if let Some(s) = sender {
                                s.send(PoseCommand::ResetPose);
                            }
                            intent_lab_apply(
                                sender,
                                pc,
                                &bones,
                                &format!("wizard probe {}", pending.label),
                                snapshot,
                                undo,
                            );
                        }
                        Err(e) => pc.status = Some(format!("wizard probe: {e}")),
                    }
                }
            }
            WizardPhase::Complete { draft, log, .. } => {
                pc.intent_lab_cal = draft.clone();
                ui.colored_label(
                    theme::success(ui),
                    format!(
                        "Wizard complete — {} step(s) recorded. Save below or tell the agent save_intent_calibration.",
                        log.len()
                    ),
                );
            }
        }
        if ui.button("Abort wizard").clicked() {
            wiz.abort();
            pc.status = Some("wizard aborted".into());
        }
    }

    ui.separator();
    ui.label(
        "Manual calibration: sliders apply to \"Try\" buttons and MCP tools after Save. \
\"Save\" writes config/semantic_intent_calibration/<key>.toml.",
    );

    egui::CollapsingHeader::new("Default rules (what each sign scales)")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(format!(
                "raise_leg forward {arrow} *UpperLeg intrinsic pitch (x raise_leg_forward_pitch_sign). \
Outward {arrow} *UpperLeg roll with left/right mirror (x raise_leg_outward_roll_sign).",
                arrow = icons::ARROW_RIGHT
            ));
            ui.label(format!(
                "bend_knee {arrow} *LowerLeg pitch (x bend_knee_pitch_sign). \
arms_down_rest {arrow} shoulder/upper-arm/lower-arm rolls & pitches (x the three arms_* signs).",
                arrow = icons::ARROW_RIGHT
            ));
            ui.label(
                "If \"forward\" reads backward, flip forward pitch sign. If outward abduction twists wrong, flip outward roll sign.",
            );
        });

    ui.separator();
    ui.label(egui::RichText::new("Calibration multipliers (−1 … +1)").strong());
    cal_sign_row(ui, &mut pc.intent_lab_cal.raise_leg_forward_pitch_sign, "raise_leg forward pitch");
    cal_sign_row(ui, &mut pc.intent_lab_cal.raise_leg_outward_roll_sign, "raise_leg outward roll");
    cal_sign_row(ui, &mut pc.intent_lab_cal.bend_knee_pitch_sign, "bend_knee pitch");
    cal_sign_row(ui, &mut pc.intent_lab_cal.arms_down_rest_upper_arm_roll_sign, "arms_down_rest upper-arm roll");
    cal_sign_row(ui, &mut pc.intent_lab_cal.arms_down_rest_elbow_pitch_sign, "arms_down_rest elbow pitch");
    cal_sign_row(ui, &mut pc.intent_lab_cal.arms_down_rest_shoulder_sign, "arms_down_rest shoulder");

    ui.horizontal(|ui| {
        if ui
            .button("Reset calibration to defaults (+1)")
            .on_hover_text("All signs → +1.0 (factory defaults).")
            .clicked()
        {
            pc.intent_lab_cal = SemanticIntentCalibration::default();
        }
        if ui.button("Save for this VRM").clicked() {
            let mut w = cal_h.0.write().unwrap();
            w.insert(key.clone(), pc.intent_lab_cal.clone());
            match w.save_file(&key, settings.avatar.model_path.as_str(), &pc.intent_lab_cal) {
                Ok(()) => pc.status = Some(format!("saved semantic calibration → {key}.toml")),
                Err(e) => pc.status = Some(format!("save failed: {e}")),
            }
        }
    });

    ui.separator();
    ui.label(egui::RichText::new("Try (same parameters as MCP)").strong());
    ui.horizontal(|ui| {
        ui.checkbox(&mut pc.intent_lab_side_left, "Left side (unchecked = right)");
        ui.checkbox(&mut pc.intent_lab_raise_forward, "raise_leg: forward (unchecked = outward)");
    });
    ui.add(egui::Slider::new(&mut pc.intent_lab_raise_amount, 0.0..=1.0).text("raise_leg amount"));
    ui.add(egui::Slider::new(&mut pc.intent_lab_bend_amount, 0.0..=1.0).text("bend_knee amount"));
    ui.add(egui::Slider::new(&mut pc.intent_lab_arms_amount, 0.0..=1.0).text("arms_down_rest amount"));

    let side = if pc.intent_lab_side_left {
        Side::Left
    } else {
        Side::Right
    };
    let raise_dir = if pc.intent_lab_raise_forward {
        Some(LegRaiseDirection::Forward)
    } else {
        Some(LegRaiseDirection::Outward)
    };

    ui.horizontal(|ui| {
        if ui.button("Dry-run raise_leg (status only)").clicked() {
            let args = RaiseLegArgs {
                side,
                amount: pc.intent_lab_raise_amount,
                direction: raise_dir,
                dry_run: Some(true),
            };
            let bones = compile_raise_leg(&args, &pc.intent_lab_cal);
            pc.status = Some(format!(
                "raise_leg dry-run euler: {:?}",
                bones
                    .iter()
                    .map(|(k, v)| (
                        k.clone(),
                        (
                            v.pitch_deg,
                            v.yaw_deg,
                            v.roll_deg,
                        )
                    ))
                    .collect::<Vec<_>>()
            ));
        }
        if ui.button("Apply raise_leg").clicked() {
            let args = RaiseLegArgs {
                side,
                amount: pc.intent_lab_raise_amount,
                direction: raise_dir,
                dry_run: Some(false),
            };
            let bones = compile_raise_leg(&args, &pc.intent_lab_cal);
            intent_lab_apply(sender, pc, &bones, "raise_leg", snapshot, undo);
        }
    });

    ui.horizontal(|ui| {
        if ui.button("Apply bend_knee").clicked() {
            let args = BendKneeArgs {
                side,
                amount: pc.intent_lab_bend_amount,
                dry_run: Some(false),
            };
            let bones = compile_bend_knee(&args, &pc.intent_lab_cal);
            intent_lab_apply(sender, pc, &bones, "bend_knee", snapshot, undo);
        }
        if ui.button("Apply arms_down_rest").clicked() {
            let args = ArmsDownRestArgs {
                amount: Some(pc.intent_lab_arms_amount),
                dry_run: Some(false),
            };
            let bones = compile_arms_down_rest(&args, &pc.intent_lab_cal);
            intent_lab_apply(sender, pc, &bones, "arms_down_rest", snapshot, undo);
        }
    });
}

fn intent_lab_apply(
    sender: Option<&PoseCommandSender>,
    pc: &mut PoseControllerUiState,
    bones: &HashMap<String, BoneEulerDeg>,
    label: &str,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    undo: Option<&UndoHistory>,
) {
    let Some(sender) = sender else {
        pc.status = Some("no PoseCommandSender".into());
        return;
    };
    if bones.is_empty() {
        pc.status = Some(format!("{label}: empty bone map"));
        return;
    }
    let safety = PoseSafetyReport::from_euler_map(bones);
    if let Some(reason) = safety.should_block(false, false) {
        pc.status = Some(format!("{label} blocked: {reason}"));
        return;
    }
    let (quats, mut w1) = bone_map_from_euler_deg(bones);
    let (sanitized, mut w2) = sanitize_bone_map(quats);
    w1.append(&mut w2);
    if let (Some(h), Some(snap)) = (undo, snapshot) {
        h.record(snap, &pc.bone_euler, format!("intent {label}"));
    }
    sender.send(PoseCommand::ApplyBones {
        bones: sanitized,
        preserve_omitted_bones: true,
        blend_weight: None,
        transition_seconds: None,
    });
    pc.status = Some(format!(
        "{label}: applied {} bone(s); warnings: {}",
        bones.len(),
        if w1.is_empty() {
            "none".into()
        } else {
            format!("{w1:?}")
        }
    ));
}

fn bone_row(
    ui: &mut egui::Ui,
    state: &mut PoseControllerUiState,
    sender: Option<&PoseCommandSender>,
    indexed: Option<&IndexedBones>,
    rig: &mut crate::plugins::rig_editor::RigEditorState,
    mirror: &crate::plugins::mirror::MirrorState,
    bone: &str,
    scroll_target: Option<&str>,
    snapshot: Option<&crate::plugins::pose_driver::BoneSnapshotHandle>,
    undo: Option<&UndoHistory>,
) {
    // Capture a pre-edit clone of the full bone_euler map for the undo entry
    // before we take a mutable borrow into it via `entry(...).or_insert(...)`.
    // Cheap (a small HashMap) and only needed once per slider drag-start.
    let pre_edit_euler = state.bone_euler.clone();
    let euler = state
        .bone_euler
        .entry(bone.to_string())
        .or_insert([0.0, 0.0, 0.0]);
    let mut x = euler[0];
    let mut y = euler[1];
    let mut z = euler[2];

    let in_index = indexed.is_some_and(|i| i.contains(bone));
    let is_selected = rig.selected_bone.as_deref() == Some(bone);
    let is_hovered_other = rig.hovered_bone.as_deref() == Some(bone) && !is_selected;

    // Light tint behind the row so list ↔ mesh hover sync is visible at a
    // glance — selected wins over hovered, hovered wins over default.
    let frame = if is_selected {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgba_unmultiplied(125, 80, 161, 36))
    } else if is_hovered_other {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgba_unmultiplied(161, 80, 146, 28))
    } else {
        egui::Frame::group(ui.style())
    };

    // Shift = precision. Reuses the same factor as viewport drag so the two
    // surfaces stay tactically consistent. Egui's slider doesn't have a
    // built-in precision modifier — apply it by reducing visible drag range
    // around the current value when Shift is held, so a full slider sweep
    // covers ±(180 * factor)° instead of ±180°.
    let shift_held = ui.input(|i| i.modifiers.shift);
    let precision = if shift_held {
        rig.shift_precision_factor.clamp(0.02, 1.0)
    } else {
        1.0
    };
    let slider_range = 180.0 * precision;
    let frame_resp = frame.show(ui, |ui| {
        let mut changed = false;
        let mut header_clicked_select = false;
        let mut header_reset_via_context = false;
        let mut header_hovered = false;

        // ---- Header row: bone name + per-row right-click context. This is
        // the ONLY clickable surface in the row — the sliders below are
        // outside this allocation so they receive their own pointer events
        // without an overlay sense stealing them.
        let header = ui.horizontal(|ui| {
            let mut label = egui::RichText::new(bone).monospace();
            if !in_index {
                label = label.color(egui::Color32::from_rgb(220, 110, 110));
            } else if is_selected {
                label = label.color(egui::Color32::from_rgb(200, 220, 255)).strong();
            } else if is_hovered_other {
                label = label.color(egui::Color32::from_rgb(230, 210, 130));
            }
            let label_resp = ui
                .button(label)
                .on_hover_text(if in_index {
                    "Click the name to select this bone in the rig editor (sets selected bone, focuses camera in edit mode). Right-click the name to reset it to rest. Sliders below stay independent."
                } else {
                    "This bone isn't in the pose driver's merged index — writes are silently dropped."
                });
            if label_resp.clicked() && in_index {
                header_clicked_select = true;
            }
            label_resp.context_menu(|ui| {
                if ui
                    .button("Reset to rest")
                    .on_hover_text(
                        "Snap this bone back to its full VRM bind transform and \
                         re-apply (0°,0°,0°) so rotation matches manual apply.",
                    )
                    .clicked()
                {
                    header_reset_via_context = true;
                    ui.close();
                }
            });
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
            if shift_held && in_index {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("⇧ ±{slider_range:.0}°"))
                            .small()
                            .color(egui::Color32::from_rgb(180, 200, 220)),
                    )
                    .on_hover_text("Shift held — slider range narrowed for precision");
                });
            }
            // Track hover on the *header* alone, not the entire row, so
            // moving the cursor across a slider doesn't hijack rig hover
            // sync.
            header_hovered = label_resp.hovered() || ui.rect_contains_pointer(label_resp.rect);
        });
        // Slider range collapses around the current value when Shift is held
        // (precision mode). Each axis uses its own min/max so a partly-rotated
        // bone can still nudge in either direction.
        let bound = |v: f32| {
            let lo = (v - slider_range).clamp(-360.0, 360.0).max(-180.0);
            let hi = (v + slider_range).clamp(-360.0, 360.0).min(180.0);
            (lo, hi)
        };
        let mut drag_started = false;
        let mut committed_without_drag = false;
        ui.horizontal(|ui| {
            ui.label("X");
            let (lo, hi) = bound(x);
            let r = ui.add(egui::Slider::new(&mut x, lo..=hi).suffix("°"));
            if r.drag_started() {
                drag_started = true;
            }
            if r.changed() {
                changed = true;
                if !r.dragged() {
                    committed_without_drag = true;
                }
            }
            ui.label("Y");
            let (lo, hi) = bound(y);
            let r = ui.add(egui::Slider::new(&mut y, lo..=hi).suffix("°"));
            if r.drag_started() {
                drag_started = true;
            }
            if r.changed() {
                changed = true;
                if !r.dragged() {
                    committed_without_drag = true;
                }
            }
            ui.label("Z");
            let (lo, hi) = bound(z);
            let r = ui.add(egui::Slider::new(&mut z, lo..=hi).suffix("°"));
            if r.drag_started() {
                drag_started = true;
            }
            if r.changed() {
                changed = true;
                if !r.dragged() {
                    committed_without_drag = true;
                }
            }
        });

        // Checkpoint once per logical edit: at the first frame of a drag, or
        // when a typed/keyboard-stepped commit lands. Skipping mid-drag
        // frames keeps the undo stack from filling with one entry per pixel.
        if drag_started || committed_without_drag {
            if let (Some(h), Some(snap)) = (undo, snapshot) {
                h.record(snap, &pre_edit_euler, format!("slider {bone}"));
            }
        }

        if changed {
            euler[0] = x;
            euler[1] = y;
            euler[2] = z;
            if let Some(s) = sender {
                send_apply_bones_euler_deg_mirrored(s, bone, [x, y, z], Some(mirror));
                state.status = Some(format!("{bone} → ({x:.1}, {y:.1}, {z:.1})"));
            }
        }

        BoneRowOutcome {
            header_clicked_select,
            header_reset_via_context,
            header_hovered: header_hovered || header.response.hovered(),
        }
    });

    let outcome = frame_resp.inner;
    let row_response = frame_resp.response;

    if outcome.header_reset_via_context {
        if let (Some(h), Some(snap)) = (undo, snapshot) {
            h.record(snap, &pre_edit_euler, format!("reset {bone}"));
        }
        state
            .bone_euler
            .insert(bone.to_string(), [0.0, 0.0, 0.0]);
        if let Some(s) = sender {
            s.send(PoseCommand::ResetBones(vec![bone.to_string()]));
            send_apply_bones_euler_deg(s, bone, [0.0, 0.0, 0.0]);
            state.status = Some(format!("{bone} → rest (right-click)"));
        }
    }

    // Hover sync uses the bone-name header only — the sliders' rects don't
    // count, otherwise dragging a slider would re-fire the rig editor's
    // hover system on every pixel.
    if in_index && outcome.header_hovered {
        rig.hovered_bone = Some(bone.to_string());
        rig.hovered_source = HoverSource::List;
    } else if rig.hovered_source == HoverSource::List
        && rig.hovered_bone.as_deref() == Some(bone)
        && !outcome.header_hovered
    {
        rig.hovered_bone = None;
        rig.hovered_source = HoverSource::None;
    }

    if outcome.header_clicked_select && in_index {
        rig.selected_bone = Some(bone.to_string());
        // Only auto-focus the camera when the user is actively editing — outside
        // edit mode, list clicks shouldn't pull the camera around. The Rig tab
        // also exposes a manual "Focus camera" button for explicit recentering.
        if rig.edit_mode {
            rig.pending_focus_camera_to_bone = Some(bone.to_string());
        }
        state.status = Some(format!("selected {bone}"));
    }

    if scroll_target == Some(bone) {
        row_response.scroll_to_me(Some(egui::Align::Center));
    }
}

/// Per-row pointer outcome bundled out of the frame closure so the post-
/// frame logic can act on it without the closure needing `&mut` borrows of
/// the rig editor / pose command state at the same time as the slider
/// widgets above.
struct BoneRowOutcome {
    header_clicked_select: bool,
    header_reset_via_context: bool,
    header_hovered: bool,
}
