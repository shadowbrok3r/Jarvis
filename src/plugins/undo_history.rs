//! Undo / redo for VRM bone state.
//!
//! Captures a normalized-humanoid-space snapshot of every indexed bone at
//! well-defined user-action points (pose-library apply, manual bone slider,
//! axis-ring drag, reset buttons, intent lab apply, channel-server
//! `vrm:apply-pose` envelopes) and replays it through the same
//! [`PoseCommand::ApplyBones`] path every other writer uses. `Ctrl-Z` pops the
//! latest checkpoint; `Ctrl-Shift-Z` (and `Ctrl-Y`) redoes.
//!
//! Continuous writers (idle VRMA, anim_layers, idle_tick, MCP `animate_*`)
//! are intentionally **not** checkpointed — they'd flood the stack every
//! frame and there is nothing meaningful for the user to "undo back to."
//! Only discrete user actions are recorded.
//!
//! The history lives behind an `Arc<Mutex<_>>` so call sites scattered across
//! nested egui closures can record checkpoints through a `Res<UndoHistory>`
//! (shared, immutable) without threading `&mut` through every function.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use bevy::prelude::*;
use bevy_egui::EguiContexts;
use parking_lot::Mutex;

use crate::plugins::debug_ui::DebugUiState;
use crate::plugins::pose_driver::{BoneSnapshotHandle, PoseCommand, PoseCommandSender};

pub struct UndoHistoryPlugin;

/// Hard cap on each stack so a long editing session doesn't keep a snapshot
/// of every drag-start in memory forever.
const HISTORY_CAPACITY: usize = 100;

#[derive(Debug, Clone)]
pub struct UndoEntry {
    /// Normalized-humanoid rotations for every indexed bone (`[x, y, z, w]`).
    pub bones: HashMap<String, [f32; 4]>,
    /// Per-bone Euler degrees the Bones-tab sliders read from. Without this
    /// the rig pops back correctly but the sliders keep showing pre-undo
    /// numbers, so the next slider drag fights the restored rotation.
    pub bone_euler: HashMap<String, [f32; 3]>,
    pub label: String,
}

#[derive(Default)]
struct UndoHistoryInner {
    undo: VecDeque<UndoEntry>,
    redo: VecDeque<UndoEntry>,
}

impl UndoHistoryInner {
    fn push_undo(&mut self, entry: UndoEntry) {
        self.redo.clear();
        if self.undo.len() >= HISTORY_CAPACITY {
            self.undo.pop_front();
        }
        self.undo.push_back(entry);
    }

    fn push_redo(&mut self, entry: UndoEntry) {
        if self.redo.len() >= HISTORY_CAPACITY {
            self.redo.pop_front();
        }
        self.redo.push_back(entry);
    }
}

/// Cloneable shared handle. Stored as a Bevy `Resource` and accessed via
/// `Res<UndoHistory>` everywhere — the inner `Mutex` lets call sites
/// scattered across nested egui closures record checkpoints without needing
/// `ResMut<>`.
#[derive(Resource, Clone, Default)]
pub struct UndoHistory(Arc<Mutex<UndoHistoryInner>>);

impl UndoHistory {
    /// Capture the current bone state as a new undo checkpoint. Cheap (one
    /// snapshot read + a small `HashMap` clone). Silently no-ops when the
    /// rig hasn't loaded yet so we never push empty entries that would wipe
    /// the rig on undo.
    pub fn record(
        &self,
        snapshot: &BoneSnapshotHandle,
        bone_euler: &HashMap<String, [f32; 3]>,
        label: impl Into<String>,
    ) {
        let bones = capture_snapshot_bones(snapshot);
        if bones.is_empty() {
            return;
        }
        self.0.lock().push_undo(UndoEntry {
            bones,
            bone_euler: bone_euler.clone(),
            label: label.into(),
        });
    }

    pub fn undo_len(&self) -> usize {
        self.0.lock().undo.len()
    }
    pub fn redo_len(&self) -> usize {
        self.0.lock().redo.len()
    }
}

fn capture_snapshot_bones(snapshot: &BoneSnapshotHandle) -> HashMap<String, [f32; 4]> {
    snapshot
        .0
        .read()
        .bones
        .iter()
        .map(|(k, v)| (k.clone(), v.rotation))
        .collect()
}

impl Plugin for UndoHistoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UndoHistory>()
            .add_systems(Update, undo_redo_keyboard_handler);
    }
}

fn undo_redo_keyboard_handler(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    sender: Option<Res<PoseCommandSender>>,
    snapshot: Option<Res<BoneSnapshotHandle>>,
    history: Res<UndoHistory>,
    mut debug: ResMut<DebugUiState>,
) {
    // Don't fire while typing into a text field.
    if matches!(contexts.ctx_mut(), Ok(ctx) if ctx.wants_keyboard_input()) {
        return;
    }
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if !ctrl {
        return;
    }
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let z = keys.just_pressed(KeyCode::KeyZ);
    let y = keys.just_pressed(KeyCode::KeyY);

    let do_undo = z && !shift;
    let do_redo = (z && shift) || y;
    if !do_undo && !do_redo {
        return;
    }

    let Some(snapshot) = snapshot.as_deref() else {
        return;
    };
    let Some(sender) = sender.as_deref() else {
        return;
    };

    let mut inner = history.0.lock();
    if do_undo {
        let Some(entry) = inner.undo.pop_back() else {
            debug.pose_controller.status = Some("nothing to undo".into());
            return;
        };
        let current_bones = capture_snapshot_bones(snapshot);
        if !current_bones.is_empty() {
            inner.push_redo(UndoEntry {
                bones: current_bones,
                bone_euler: debug.pose_controller.bone_euler.clone(),
                label: entry.label.clone(),
            });
        }
        drop(inner);
        apply_entry(sender, &entry, &mut debug, "undo");
    } else if do_redo {
        let Some(entry) = inner.redo.pop_back() else {
            debug.pose_controller.status = Some("nothing to redo".into());
            return;
        };
        let current_bones = capture_snapshot_bones(snapshot);
        if !current_bones.is_empty() {
            // Push onto undo without clearing redo (which `push_undo` does).
            if inner.undo.len() >= HISTORY_CAPACITY {
                inner.undo.pop_front();
            }
            inner.undo.push_back(UndoEntry {
                bones: current_bones,
                bone_euler: debug.pose_controller.bone_euler.clone(),
                label: entry.label.clone(),
            });
        }
        drop(inner);
        apply_entry(sender, &entry, &mut debug, "redo");
    }
}

fn apply_entry(
    sender: &PoseCommandSender,
    entry: &UndoEntry,
    debug: &mut DebugUiState,
    direction: &str,
) {
    sender.send(PoseCommand::ApplyBones {
        bones: entry.bones.clone(),
        preserve_omitted_bones: true,
        blend_weight: Some(1.0),
        transition_seconds: Some(0.0),
    });
    debug.pose_controller.bone_euler = entry.bone_euler.clone();
    debug.pose_controller.status = Some(format!(
        "{direction}: {} ({} bones)",
        entry.label,
        entry.bones.len()
    ));
}
