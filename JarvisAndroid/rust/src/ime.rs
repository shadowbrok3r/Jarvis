//! Soft-keyboard text input through EguiMobile's hidden-`EditText` bridge.
//!
//! Opt-in: build with `--features ime-bridge`. Without it, text input still
//! works via `bevy_egui`'s `process_ime_system`, which drives winit's
//! `set_ime_allowed` and gets an implicit DecorView keyboard. That path handles
//! ordinary typing but has no `InputConnection`, so it has no swipe typing, no
//! Gboard spacebar-trackpad cursor, and no IME composition for CJK.
//!
//! The two paths are mutually exclusive. `EguiGlobalSettings::enable_ime` is set
//! to `false` in `lib.rs` when this feature is on: an implicit
//! `hide_soft_input` on the DecorView token kills a keyboard served by our
//! `EditText`, and the follow-up implicit show is then ignored ("view is not
//! served").
//!
//! Ported from `egui-android`'s eframe `Adapter` (`lib.rs:73-396`), minus the
//! machinery that exists purely to survive `egui-winit`'s `set_ime_allowed`
//! bounce — `enable_ime = false` removes that adversary, so `last_ime` pinning
//! and the open/hidden recovery counters are deliberately not ported.

use bevy::prelude::*;
use bevy_egui::egui;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass};
use egui_android_ime::bridge;

/// Frames without a focused field before the keyboard is dismissed. ~0.5 s at
/// 60 fps, which absorbs one-frame focus flickers during keyboard reflow.
const HIDE_ARM_FRAMES: u32 = 30;

pub struct AndroidImePlugin;

impl Plugin for AndroidImePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ImeState>()
            .add_systems(PreUpdate, reset_frame_latch)
            .add_systems(
                EguiPrimaryContextPass,
                (
                    ime_pre_ui.before(crate::ui::UiDrawSet),
                    ime_post_ui.after(crate::ui::UiDrawSet),
                ),
            );
    }
}

#[derive(Resource, Default)]
struct ImeState {
    initialised: bool,
    /// True once `set_soft_keyboard(true)` has been sent and not yet undone.
    hot: bool,
    last_focus: Option<egui::Id>,
    /// Events pulled from the `InputConnection`, awaiting injection into egui.
    pending: Vec<egui::Event>,
    /// Push the whole document to the `EditText` on the next opportunity.
    force_sync: bool,
    /// The next seed should restart the IME session, not just update text.
    seed_restart: bool,
    /// Frames the keyboard has been unwanted; see [`HIDE_ARM_FRAMES`].
    hide_arm: u32,
    /// `EguiPlugin` runs the pass schedule more than once per frame in
    /// multipass mode; the queue must only be drained on the first.
    drained: bool,
    /// Set when this frame applied IME events, so the caret mirror is skipped
    /// while egui's own cursor is still converging.
    applied: bool,
}

/// Clears the once-per-frame latch before the egui pass runs.
fn reset_frame_latch(mut state: ResMut<ImeState>) {
    state.drained = false;
    state.applied = false;
}

/// Drains the `InputConnection` into egui, before the overlay draws.
fn ime_pre_ui(mut contexts: EguiContexts, mut state: ResMut<ImeState>) {
    let Ok(ctx) = contexts.ctx_mut() else { return };
    let ctx = ctx.clone();

    if !state.initialised {
        let Some(app) = bevy::android::ANDROID_APP.get() else {
            return;
        };
        egui_android_ime::init(app.vm_as_ptr(), app.activity_as_ptr());
        bridge::register_natives();
        // InputConnection callbacks land on the Android UI thread and produce no
        // winit event, so without this the loop can sleep through typed input.
        bridge::set_wake_context(&ctx);
        state.initialised = true;
        info!("ime bridge initialised");
    }

    if state.drained {
        return;
    }
    state.drained = true;

    if state.hot {
        bridge::bind_ime();
        let mut pending = std::mem::take(&mut state.pending);
        state.applied = bridge::apply_pending(&ctx, state.last_focus, &mut pending);
        state.pending = pending;
        // Backstop for a missed nativeImeWake, or an event landing mid-frame.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    if !state.pending.is_empty() {
        let events = std::mem::take(&mut state.pending);
        // Both queues: `raw` is what egui replays, `events` is what this
        // already-begun pass reads. Appending to only one drops the input.
        ctx.input_mut(|i| {
            i.raw.events.extend(events.iter().cloned());
            i.events.extend(events);
        });
    }

    // Without this the field blurs between characters: "first letter, then
    // silence until retap".
    if state.hot {
        ctx.options_mut(|o| o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
        pin_focus(&ctx, state.last_focus);
    }
}

/// Mirrors egui's focus and text state back to the `EditText` after the overlay ran.
fn ime_post_ui(mut contexts: EguiContexts, mut state: ResMut<ImeState>) {
    let Ok(ctx) = contexts.ctx_mut() else { return };
    let ctx = ctx.clone();
    let focused = ctx.memory(|m| m.focused());
    let prev_focus = state.last_focus;

    if focused.is_some() {
        state.last_focus = focused;
    } else if !state.hot {
        state.last_focus = None;
    }

    let switched = matches!((prev_focus, state.last_focus), (Some(a), Some(b)) if a != b);
    if switched {
        // Events deferred against the old field must not replay into the new one.
        bridge::clear_preedit_tracking();
        bridge::clear_carry();
        state.seed_restart = true;
    }

    // A field taking focus can carry a stuck composition from an earlier tap;
    // egui paints no caret while composing and it never self-heals.
    if let Some(id) = state.last_focus
        && prev_focus != state.last_focus
    {
        if let Some(mut st) = egui::text_edit::TextEditState::load(&ctx, id)
            && let Some(range) = st.cursor.char_range()
        {
            let r = range.as_sorted_char_range();
            if r.start != r.end {
                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(r.end),
                )));
                st.store(&ctx, id);
            }
        }
        state.pending.push(egui::Event::Ime(egui::ImeEvent::Preedit {
            text: String::new(),
            active_range_chars: None,
        }));
    }

    // The user dismissed the keyboard with Back.
    if state.hot && bridge::take_dismissed() {
        teardown(&ctx, &mut state);
        return;
    }

    if focused.is_some() {
        state.hide_arm = 0;
        if !state.hot {
            bridge::set_soft_keyboard(true);
            state.hot = true;
            // Seed the EditText once on open, never per keystroke: setText
            // resets the caret and triggers invalidateInput.
            state.force_sync = true;
        } else {
            bridge::bind_ime();
        }
    } else if state.hot {
        state.hide_arm = state.hide_arm.saturating_add(1);
        if state.hide_arm >= HIDE_ARM_FRAMES {
            teardown(&ctx, &mut state);
            return;
        }
    }

    state.seed_restart |= bridge::take_reseed_restart();
    let need_sync = state.force_sync || switched || bridge::take_needs_reseed();
    if need_sync {
        bridge::invalidate_last_sync();
        let seeded = if state.seed_restart {
            bridge::sync_focused_text_edit_restart(&ctx, state.last_focus)
        } else {
            bridge::sync_focused_text_edit(&ctx, state.last_focus)
        };
        if seeded {
            state.force_sync = false;
            state.seed_restart = false;
        } else if state.hot && state.last_focus.is_some() {
            // The undoer has no stable snapshot yet; seeding now would push ""
            // and every later IME op would edit against an empty mirror.
            state.force_sync = true;
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            state.force_sync = false;
            state.seed_restart = false;
        }
    }

    // Mirror egui's caret into the EditText when it drifts. Skipped on frames
    // that applied IME events, where egui's cursor is still converging.
    if state.hot
        && !need_sync
        && !state.applied
        && let Some(id) = state.last_focus
        && let Some(st) = egui::text_edit::TextEditState::load(&ctx, id)
        && st.cursor.char_range().is_some()
        && !bridge::resync_out_of_band(&ctx, state.last_focus)
    {
        let (s, e) = bridge::selection_chars(&st);
        let user_tap = ctx.input(|i| i.pointer.any_pressed() || i.pointer.any_released());
        bridge::sync_caret_to_ime(s, e, user_tap);
    }

    // Re-pin after the UI so reflow cannot leave us unfocused for the next char.
    if state.hot {
        pin_focus(&ctx, state.last_focus);
    }
}

fn pin_focus(ctx: &egui::Context, focus: Option<egui::Id>) {
    let Some(id) = focus else { return };
    if ctx.memory(|m| m.focused()) == Some(id) {
        return;
    }
    ctx.memory_mut(|m| m.request_focus(id));
}

fn teardown(ctx: &egui::Context, state: &mut ImeState) {
    if let Some(id) = state.last_focus {
        ctx.memory_mut(|m| m.surrender_focus(id));
    }
    bridge::set_soft_keyboard(false);
    bridge::clear_preedit_tracking();
    bridge::clear_carry();
    state.hot = false;
    state.hide_arm = 0;
    state.force_sync = false;
    state.seed_restart = false;
    state.last_focus = None;
}
