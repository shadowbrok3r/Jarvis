import BridgeFFI

public func jarvis_ios_version() -> RustString {
    RustString(ptr: __swift_bridge__$jarvis_ios_version())
}
public func jarvis_renderer_new(_ ui_view: UnsafeMutablePointer<UInt8>, _ width_px: UInt32, _ height_px: UInt32, _ pixels_per_point: Float) -> UnsafeMutablePointer<UInt8> {
    __swift_bridge__$jarvis_renderer_new(ui_view, width_px, height_px, pixels_per_point)
}
public func jarvis_renderer_free(_ ptr: UnsafeMutablePointer<UInt8>) {
    __swift_bridge__$jarvis_renderer_free(ptr)
}
public func jarvis_renderer_render(_ ptr: UnsafeMutablePointer<UInt8>, _ time_seconds: Double) {
    __swift_bridge__$jarvis_renderer_render(ptr, time_seconds)
}
public func jarvis_renderer_resize(_ ptr: UnsafeMutablePointer<UInt8>, _ width_px: UInt32, _ height_px: UInt32) {
    __swift_bridge__$jarvis_renderer_resize(ptr, width_px, height_px)
}
public func jarvis_renderer_touch(_ ptr: UnsafeMutablePointer<UInt8>, _ phase: UInt8, _ x: Float, _ y: Float, _ id: UInt64) {
    __swift_bridge__$jarvis_renderer_touch(ptr, phase, x, y, id)
}
public func jarvis_renderer_reload_profile(_ ptr: UnsafeMutablePointer<UInt8>) {
    __swift_bridge__$jarvis_renderer_reload_profile(ptr)
}
public func jarvis_renderer_queue_vrma(_ ptr: UnsafeMutablePointer<UInt8>, _ path_ptr: UnsafePointer<UInt8>, _ path_len: UInt, _ loop_forever: UInt8) {
    __swift_bridge__$jarvis_renderer_queue_vrma(ptr, path_ptr, path_len, loop_forever)
}
public func jarvis_ios_debug_log_snapshot() -> RustString {
    RustString(ptr: __swift_bridge__$jarvis_ios_debug_log_snapshot())
}
public func jarvis_ios_debug_log_clear() {
    __swift_bridge__$jarvis_ios_debug_log_clear()
}


