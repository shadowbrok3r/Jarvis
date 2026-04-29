//! `staticlib` + swift-bridge for JarvisIOS. On iOS, [`ios_bevy`] runs Bevy inside a `UIView`.

mod debug_log;

#[cfg(target_os = "ios")]
mod ios_graphics;
#[cfg(target_os = "ios")]
mod ios_profile_manifest;
#[cfg(target_os = "ios")]
mod ios_spring_preset;
#[cfg(target_os = "ios")]
mod jarvis_egui_theme;
#[cfg(target_os = "ios")]
mod ios_egui_ui;
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
