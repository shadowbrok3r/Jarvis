//! `staticlib` + swift-bridge for JarvisIOS. On iOS, [`ios_bevy`] runs Bevy inside a `UIView`.

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
    }
}

pub fn jarvis_ios_version() -> String {
    format!("jarvis_ios {}", env!("CARGO_PKG_VERSION"))
}

// ── Renderer FFI (UIKit `UIView` pointer; stubs on non-iOS for host `cargo check`) ─────────────

#[cfg(target_os = "ios")]
pub fn jarvis_renderer_new(
    ui_view: *mut u8,
    width_px: u32,
    height_px: u32,
    pixels_per_point: f32,
) -> *mut u8 {
    match ios_bevy::IosEmbeddedRenderer::new(ui_view.cast(), width_px, height_px, pixels_per_point) {
        Some(r) => Box::into_raw(Box::new(r)).cast(),
        None => core::ptr::null_mut(),
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
    unsafe {
        (*ptr.cast::<ios_bevy::IosEmbeddedRenderer>()).render();
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
