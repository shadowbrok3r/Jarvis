//! `staticlib` + swift-bridge for JarvisIOS. On iOS, [`ios_bevy`] runs Bevy inside a `UIView`.

mod debug_log;

#[cfg(target_os = "ios")]
mod ios_graphics;
#[cfg(target_os = "ios")]
mod ios_profile_manifest;
#[cfg(target_os = "ios")]
mod ios_spring_preset;
#[cfg(target_os = "ios")]
mod ios_mtoon_overrides;
#[cfg(target_os = "ios")]
mod ios_material_visibility;
#[cfg(target_os = "ios")]
mod ios_user_prefs;
#[cfg(target_os = "ios")]
mod jarvis_egui_theme;
#[cfg(target_os = "ios")]
mod ios_egui_ui;
#[cfg(target_os = "ios")]
mod ios_anim_json;
#[cfg(target_os = "ios")]
mod ios_anim_layers;
#[cfg(target_os = "ios")]
mod ios_device_motion;
#[cfg(target_os = "ios")]
mod ios_mem_probe;
#[cfg(target_os = "ios")]
mod ios_bevy;

/// Opaque pointers cross the bridge as `*mut u8` (Swift: `UnsafeMutableRawPointer`).
#[swift_bridge::bridge]
mod ffi {
    extern "Rust" {
        fn jarvis_ios_version() -> String;

        fn jarvis_renderer_new(
            ui_view: *mut u8,
            width_px: u32,
            height_px: u32,
            pixels_per_point: f32,
        ) -> *mut u8;

        fn jarvis_renderer_free(ptr: *mut u8);

        fn jarvis_renderer_render(ptr: *mut u8, time_seconds: f64);

        fn jarvis_renderer_resize(ptr: *mut u8, width_px: u32, height_px: u32);

        fn jarvis_renderer_touch(ptr: *mut u8, phase: u8, x: f32, y: f32, id: u64);

        fn jarvis_renderer_reload_profile(ptr: *mut u8);

        fn jarvis_renderer_queue_vrma(ptr: *mut u8, path_ptr: *const u8, path_len: usize, loop_forever: u8);

        fn jarvis_renderer_queue_anim_json(ptr: *mut u8, path_ptr: *const u8, path_len: usize, loop_forever: u8);

        fn jarvis_renderer_set_device_motion(
            ptr: *mut u8,
            gx: f32,
            gy: f32,
            gz: f32,
            ax: f32,
            ay: f32,
            az: f32,
            enabled: u8,
        );

        fn jarvis_renderer_set_device_motion_tuning(
            ptr: *mut u8,
            gravity_blend: f32,
            max_tilt_deg: f32,
            shake_power: f32,
            max_shake_mult: f32,
            shake_deadzone: f32,
        );

        fn jarvis_renderer_expressions_snapshot_json(ptr: *mut u8) -> String;

        fn jarvis_renderer_set_expression_weight(
            ptr: *mut u8,
            name_ptr: *const u8,
            name_len: usize,
            weight: f32,
        );

        fn jarvis_renderer_apply_expressions(ptr: *mut u8);

        fn jarvis_renderer_layers_snapshot_json(ptr: *mut u8) -> String;

        fn jarvis_renderer_layers_set_master(ptr: *mut u8, enabled: u8);

        fn jarvis_renderer_layers_install_default(ptr: *mut u8);

        fn jarvis_renderer_layers_set_enabled(ptr: *mut u8, layer_id: u64, enabled: u8);

        fn jarvis_renderer_layers_set_weight(ptr: *mut u8, layer_id: u64, weight: f32);

        fn jarvis_renderer_layers_clear(ptr: *mut u8);

        fn jarvis_ios_debug_log_snapshot() -> String;

        fn jarvis_ios_debug_log_clear();
    }
}

pub fn jarvis_ios_version() -> String {
    format!("jarvis_ios {}", env!("CARGO_PKG_VERSION"))
}

pub fn jarvis_ios_debug_log_snapshot() -> String {
    debug_log::jarvis_ios_debug_log_snapshot()
}

pub fn jarvis_ios_debug_log_clear() {
    debug_log::jarvis_ios_debug_log_clear();
}

// ── Renderer FFI (UIKit `UIView` pointer; stubs on non-iOS for host `cargo check`) ─────────────

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_new(
    ui_view: *mut u8,
    width_px: u32,
    height_px: u32,
    pixels_per_point: f32,
) -> *mut u8 {
    crate::jarvis_ios_line!(
        "[JarvisIOS] jarvis_renderer_new enter ui_view={:p} size={}x{} px_per_pt={}",
        ui_view,
        width_px,
        height_px,
        pixels_per_point
    );
    match ios_bevy::IosEmbeddedRenderer::new(ui_view.cast(), width_px, height_px, pixels_per_point) {
        Some(r) => {
            crate::jarvis_ios_line!("[JarvisIOS] jarvis_renderer_new OK (IosEmbeddedRenderer allocated)");
            Box::into_raw(Box::new(r)).cast()
        }
        None => {
            crate::jarvis_ios_line!(
                "[JarvisIOS] jarvis_renderer_new FAILED: IosEmbeddedRenderer::new returned None (null UIView? or Bevy init panic)"
            );
            core::ptr::null_mut()
        }
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_new(
    _ui_view: *mut u8,
    _width_px: u32,
    _height_px: u32,
    _pixels_per_point: f32,
) -> *mut u8 {
    core::ptr::null_mut()
}

pub fn jarvis_renderer_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    #[cfg(target_os = "ios")]
    unsafe {
        drop(Box::from_raw(ptr.cast::<ios_bevy::IosEmbeddedRenderer>()));
    }
}

pub fn jarvis_renderer_render(ptr: *mut u8, _time_seconds: f64) {
    if ptr.is_null() {
        return;
    }
    #[cfg(target_os = "ios")]
    {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let r = ptr.cast::<ios_bevy::IosEmbeddedRenderer>();
        let result = catch_unwind(AssertUnwindSafe(|| unsafe { (*r).render() }));
        if let Err(payload) = result {
            unsafe {
                (*r).note_render_poisoned();
            }
            let msg = payload
                .downcast_ref::<&'static str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("(non-string panic payload)");
            crate::jarvis_ios_line!(
                "[JarvisIOS] jarvis_renderer_render: caught Rust panic (would abort across Swift FFI). msg={}",
                msg
            );
        }
    }
}

pub fn jarvis_renderer_resize(ptr: *mut u8, width_px: u32, height_px: u32) {
    if ptr.is_null() {
        return;
    }
    #[cfg(target_os = "ios")]
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).resize(width_px, height_px);
    }
    #[cfg(not(target_os = "ios"))]
    let _ = (ptr, width_px, height_px);
}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_touch(ptr: *mut u8, phase: u8, x: f32, y: f32, id: u64) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).queue_touch(phase, x, y, id);
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_touch(_ptr: *mut u8, _phase: u8, _x: f32, _y: f32, _id: u64) {}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_reload_profile(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).queue_profile_reload();
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_reload_profile(_ptr: *mut u8) {}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_queue_vrma(
    ptr: *mut u8,
    path_ptr: *const u8,
    path_len: usize,
    loop_forever: u8,
) {
    if ptr.is_null() || path_ptr.is_null() || path_len == 0 {
        return;
    }
    let path = unsafe { std::slice::from_raw_parts(path_ptr, path_len) };
    let Ok(s) = std::str::from_utf8(path) else {
        return;
    };
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).queue_vrma_play(s.to_owned(), loop_forever != 0);
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_queue_vrma(
    _ptr: *mut u8,
    _path_ptr: *const u8,
    _path_len: usize,
    _loop_forever: u8,
) {
}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_queue_anim_json(
    ptr: *mut u8,
    path_ptr: *const u8,
    path_len: usize,
    loop_forever: u8,
) {
    if ptr.is_null() || path_ptr.is_null() || path_len == 0 {
        return;
    }
    let path = unsafe { std::slice::from_raw_parts(path_ptr, path_len) };
    let Ok(s) = std::str::from_utf8(path) else {
        return;
    };
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).queue_json_anim_play(s.to_owned(), loop_forever != 0);
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_queue_anim_json(
    _ptr: *mut u8,
    _path_ptr: *const u8,
    _path_len: usize,
    _loop_forever: u8,
) {
}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_set_device_motion(
    ptr: *mut u8,
    gx: f32,
    gy: f32,
    gz: f32,
    ax: f32,
    ay: f32,
    az: f32,
    enabled: u8,
) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).set_device_motion(
            gx, gy, gz, ax, ay, az, enabled != 0,
        );
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_set_device_motion(
    _ptr: *mut u8,
    _gx: f32,
    _gy: f32,
    _gz: f32,
    _ax: f32,
    _ay: f32,
    _az: f32,
    _enabled: u8,
) {
}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_set_device_motion_tuning(
    ptr: *mut u8,
    gravity_blend: f32,
    max_tilt_deg: f32,
    shake_power: f32,
    max_shake_mult: f32,
    shake_deadzone: f32,
) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).set_device_motion_tuning(
            gravity_blend,
            max_tilt_deg,
            shake_power,
            max_shake_mult,
            shake_deadzone,
        );
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_set_device_motion_tuning(
    _ptr: *mut u8,
    _gravity_blend: f32,
    _max_tilt_deg: f32,
    _shake_power: f32,
    _max_shake_mult: f32,
    _shake_deadzone: f32,
) {
}

fn utf8_from_ptr(path_ptr: *const u8, path_len: usize) -> Option<String> {
    if path_ptr.is_null() || path_len == 0 {
        return None;
    }
    let path = unsafe { std::slice::from_raw_parts(path_ptr, path_len) };
    std::str::from_utf8(path).ok().map(str::to_owned)
}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_expressions_snapshot_json(ptr: *mut u8) -> String {
    if ptr.is_null() {
        return "{}".into();
    }
    unsafe { (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).expressions_snapshot_json() }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_expressions_snapshot_json(_ptr: *mut u8) -> String {
    "{}".into()
}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_set_expression_weight(ptr: *mut u8, name_ptr: *const u8, name_len: usize, weight: f32) {
    let Some(name) = utf8_from_ptr(name_ptr, name_len) else { return };
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).set_expression_weight(&name, weight);
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_set_expression_weight(
    _ptr: *mut u8,
    _name_ptr: *const u8,
    _name_len: usize,
    _weight: f32,
) {
}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_apply_expressions(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).apply_expressions_from_state();
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_apply_expressions(_ptr: *mut u8) {}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_layers_snapshot_json(ptr: *mut u8) -> String {
    if ptr.is_null() {
        return "{}".into();
    }
    unsafe { (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).layers_snapshot_json() }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_layers_snapshot_json(_ptr: *mut u8) -> String {
    "{}".into()
}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_layers_set_master(ptr: *mut u8, enabled: u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).layers_set_master(enabled != 0);
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_layers_set_master(_ptr: *mut u8, _enabled: u8) {}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_layers_install_default(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).layers_install_default();
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_layers_install_default(_ptr: *mut u8) {}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_layers_set_enabled(ptr: *mut u8, layer_id: u64, enabled: u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).layers_set_enabled(layer_id, enabled != 0);
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_layers_set_enabled(_ptr: *mut u8, _layer_id: u64, _enabled: u8) {}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_layers_set_weight(ptr: *mut u8, layer_id: u64, weight: f32) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).layers_set_weight(layer_id, weight);
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_layers_set_weight(_ptr: *mut u8, _layer_id: u64, _weight: f32) {}

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_layers_clear(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).layers_clear();
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_renderer_layers_clear(_ptr: *mut u8) {}
