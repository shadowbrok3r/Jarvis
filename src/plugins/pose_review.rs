//! Pose-approval middleware — a human-in-the-loop gate.
//!
//! The agent applies a pose to the avatar, then calls the MCP `review_pose`
//! tool. That tool opens a pending review here and blocks (polling the shared
//! handle) until the operator answers in the egui approval window:
//!
//! * **Yes** → the pose is good; the tool returns `approved: true` (plus an
//!   optional "overwrite as canonical" flag so the agent can re-save it).
//! * **No** → the operator types what's wrong; the tool returns
//!   `approved: false` + that feedback so the agent can tweak and re-check.
//!
//! The loop (apply → review → tweak → re-review) repeats until the operator
//! clicks Yes. The handle is an `Arc<Mutex<…>>` shared between the Bevy main
//! thread (UI writes the verdict) and the MCP Tokio runtime (tool reads it).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};

use crate::{icons, theme};

/// A review the operator has not yet answered.
#[derive(Debug, Clone)]
pub struct PoseReviewRequest {
    pub id: u64,
    /// Pose name (or generated-clip name) under review.
    pub pose_name: String,
    /// The VISUAL goal — what the pose should look like. Rendered prominently
    /// so the operator can sculpt the rig to match the agent's intent.
    pub intent: String,
    /// Operational note (e.g. "layer stack disabled", "review the live rig").
    /// Rendered small / de-emphasized, separate from `intent`.
    pub note: String,
}

/// The operator's verdict for a specific review id.
#[derive(Debug, Clone)]
pub struct PoseReviewResult {
    pub id: u64,
    /// Pose name this verdict is for (carried so a poll tool can finish the
    /// overwrite without re-deriving it from the request).
    pub pose_name: String,
    pub approved: bool,
    /// What's wrong (only meaningful when `approved == false`).
    pub feedback: String,
    /// Operator asked to overwrite/promote this pose as canonical.
    pub overwrite: bool,
}

/// Shared review state. Exactly one review is in flight at a time.
#[derive(Debug, Default)]
pub struct PoseReviewState {
    pub pending: Option<PoseReviewRequest>,
    pub result: Option<PoseReviewResult>,
    next_id: u64,
    // transient UI scratch (written by the egui window each frame)
    ui_feedback: String,
    ui_overwrite: bool,
}

impl PoseReviewState {
    /// Open a new review, returning its id. Clears stale UI scratch + result.
    pub fn open(&mut self, pose_name: String, intent: String, note: String) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.pending = Some(PoseReviewRequest {
            id,
            pose_name,
            intent,
            note,
        });
        self.result = None;
        self.ui_feedback.clear();
        self.ui_overwrite = false;
        id
    }

    /// Record the operator's verdict for the in-flight review.
    pub fn resolve(&mut self, approved: bool, feedback: String, overwrite: bool) {
        if let Some(req) = self.pending.take() {
            self.result = Some(PoseReviewResult {
                id: req.id,
                pose_name: req.pose_name,
                approved,
                feedback,
                overwrite,
            });
        }
    }

    /// Take the result for `id` if it has been answered. Non-destructive for
    /// other ids (a late poll after a new review opened returns `None`).
    pub fn take_result(&mut self, id: u64) -> Option<PoseReviewResult> {
        if self.result.as_ref().map(|r| r.id) == Some(id) {
            return self.result.take();
        }
        None
    }

    /// Take whatever answered result is sitting, regardless of id (used by the
    /// generic poll tool so a late answer is never dropped).
    pub fn take_any_result(&mut self) -> Option<PoseReviewResult> {
        self.result.take()
    }

    /// The currently-open (unanswered) review, if any.
    pub fn pending(&self) -> Option<PoseReviewRequest> {
        self.pending.clone()
    }
}

/// Cloneable, thread-safe handle to [`PoseReviewState`]. Inserted as a Bevy
/// resource and cloned into the MCP server.
#[derive(Resource, Clone)]
pub struct PoseReviewHandle(pub Arc<Mutex<PoseReviewState>>);

impl Default for PoseReviewHandle {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(PoseReviewState::default())))
    }
}

/// Registers the shared handle + the egui approval window.
pub struct PoseReviewPlugin;

impl Plugin for PoseReviewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PoseReviewHandle>()
            .add_systems(EguiPrimaryContextPass, draw_pose_review_window);
    }
}

fn draw_pose_review_window(mut contexts: EguiContexts, handle: Res<PoseReviewHandle>) {
    // Snapshot what we need under the lock, then drop it before touching egui.
    let req = {
        let st = handle.0.lock().unwrap();
        st.pending.clone()
    };
    let Some(req) = req else {
        return;
    };
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let mut verdict: Option<(bool, String, bool)> = None;
    let (mut feedback, mut overwrite) = {
        let st = handle.0.lock().unwrap();
        (st.ui_feedback.clone(), st.ui_overwrite)
    };

    egui::Window::new(format!("{} Pose review", icons::INFO))
        .collapsible(false)
        .resizable(true)
        .default_width(440.0)
        .anchor(egui::Align2::CENTER_TOP, [0.0, 48.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Is this pose good?");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.colored_label(theme::weak_text(ui), &req.pose_name);
                });
            });

            // Prominent "intended look" panel — the visual goal, set apart from
            // the operational note so the operator can sculpt to match it.
            let intent = if req.intent.trim().is_empty() {
                "(no intent description provided)"
            } else {
                req.intent.as_str()
            };
            egui::Frame::group(ui.style())
                .fill(ui.visuals().faint_bg_color)
                .stroke(egui::Stroke::new(1.0_f32, theme::accent(ui)))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        egui::RichText::new("INTENDED LOOK")
                            .small()
                            .strong()
                            .color(theme::accent(ui)),
                    );
                    ui.label(egui::RichText::new(intent).size(15.0));
                });

            if !req.note.trim().is_empty() {
                ui.add_space(2.0);
                ui.colored_label(theme::weak_text(ui), &req.note);
            }
            ui.separator();

            ui.checkbox(&mut overwrite, "overwrite as canonical on approve")
                .on_hover_text("When approved, the agent re-saves the current avatar pose (sculpt it yourself to match the intent first) under this name as the corrected version.");

            ui.horizontal(|ui| {
                ui.label("if not:");
                ui.add(
                    egui::TextEdit::singleline(&mut feedback)
                        .hint_text("what's wrong / what you changed…")
                        .desired_width(f32::INFINITY),
                );
            });

            ui.separator();
            ui.horizontal(|ui| {
                let yes = ui.add(
                    egui::Button::new(
                        egui::RichText::new(format!("{} Yes", icons::STATUS_READY))
                            .color(theme::success(ui)),
                    ),
                );
                let no = ui.add_enabled(
                    !feedback.trim().is_empty(),
                    egui::Button::new(
                        egui::RichText::new(format!("{} No", icons::STATUS_ERR))
                            .color(theme::error(ui)),
                    ),
                )
                .on_disabled_hover_text("Type what's wrong first so the agent can fix it.");
                if yes.clicked() {
                    verdict = Some((true, String::new(), overwrite));
                } else if no.clicked() {
                    verdict = Some((false, feedback.clone(), overwrite));
                }
            });
        });

    // Persist scratch + apply any verdict under the lock.
    let mut st = handle.0.lock().unwrap();
    st.ui_feedback = feedback;
    st.ui_overwrite = overwrite;
    if let Some((approved, fb, ow)) = verdict {
        st.resolve(approved, fb, ow);
    }
}
