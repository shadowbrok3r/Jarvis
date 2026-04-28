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
public func jarvis_ios_debug_log_snapshot() -> RustString {
    RustString(ptr: __swift_bridge__$jarvis_ios_debug_log_snapshot())
}
public func jarvis_ios_debug_log_clear() {
    __swift_bridge__$jarvis_ios_debug_log_clear()
}
