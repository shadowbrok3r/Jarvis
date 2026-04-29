import Foundation
import QuartzCore
import SwiftUI
import UIKit

/// Holds the active Metal/Bevy renderer pointer so hub sync can hot-reload profile **without** bumping `sessionKey`.
/// Defined in this file (not a separate source) so every SwiftPM / xtool checkout compiles it with the `JarvisIOS` target.
@MainActor
enum JarvisBevySession {
    /// Not `weak` — `weak` only applies to class references; this is a raw Rust/FFI pointer we clear in `unregisterRenderer` / teardown.
    private static var renderer: UnsafeMutablePointer<UInt8>?

    static func registerRenderer(_ ptr: UnsafeMutablePointer<UInt8>?) {
        renderer = ptr
    }

    static func unregisterRenderer() {
        renderer = nil
    }

    static func reloadProfileFromDiskManifest() {
        guard let r = renderer else {
            JarvisIOSLog.recordBevy("reloadProfile: no renderer (Avatar tab never started?)")
            return
        }
        HubProfileSync.applyIosAvatarOverrideEnvFromUserDefaults()
        jarvis_renderer_reload_profile(r)
        JarvisIOSLog.recordBevy("reloadProfile: queued (next frame)")
    }

    /// `path` is relative to `JARVIS_ASSET_ROOT` (e.g. `models/wave.vrma`).
    static func queueVrma(path: String, loopForever: Bool) {
        guard let r = renderer else {
            JarvisIOSLog.recordBevy("queueVrma: no renderer")
            return
        }
        let utf8 = Array(path.utf8)
        utf8.withUnsafeBufferPointer { buf in
            guard let base = buf.baseAddress else { return }
            jarvis_renderer_queue_vrma(r, base, UInt(buf.count), loopForever ? 1 : 0)
        }
    }
}

/// Forwards multi-touch to Bevy (`TouchInput` injection). `UIView.contentScaleFactor` is sometimes `1`
/// under SwiftUI; use `max(contentScaleFactor, screen.scale)` for Retina-sized Metal surfaces.
@MainActor
private protocol JarvisBevyTouchSink: AnyObject {
    func jarvisDeliverTouch(phase: UInt8, x: CGFloat, y: CGFloat, id: UInt64)
}

private final class JarvisBevyBackingView: UIView {
    weak var touchSink: JarvisBevyTouchSink?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        isMultipleTouchEnabled = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesBegan(touches, with: event)
        deliver(touches, phase: 0)
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesMoved(touches, with: event)
        deliver(touches, phase: 1)
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesEnded(touches, with: event)
        deliver(touches, phase: 2)
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesCancelled(touches, with: event)
        deliver(touches, phase: 3)
    }

    private func deliver(_ touches: Set<UITouch>, phase: UInt8) {
        guard let sink = touchSink else { return }
        for t in touches {
            let p = t.location(in: self)
            let id = UInt64(UInt(bitPattern: Unmanaged.passUnretained(t).toOpaque()))
            sink.jarvisDeliverTouch(phase: phase, x: p.x, y: p.y, id: id)
        }
    }
}

/// Hosts the Rust Bevy renderer in a `UIView` (Metal via wgpu); drives `jarvis_renderer_*` on the main thread.
///
/// Use with an explicit `.frame` from `GeometryReader` (see `MainShellView`): `TabView` often proposes 0×0
/// for `UIViewRepresentable`, which never starts `jarvis_renderer_new`.
///
/// Bump `sessionKey` (e.g. after hub sync) to tear down Metal and bootstrap again **without** SwiftUI
/// `.id(...)` — recreating the `UIView` races two coordinators (0×0 vs real bounds, cancelled `Task`).
///
/// Set `avatarTabVisible` from the shell tab: **do not** bootstrap or tick `CADisplayLink` while Chat/About/Logs
/// is in front. The view still receives full `GeometryReader` size behind other tabs; driving Metal/Bevy
/// there is unstable (e.g. LiveContainer) and can **SIGABRT** on the first frame after hub sync + tab switch.
@MainActor
struct JarvisBevyView: UIViewRepresentable {
    /// Increment when hub assets / env should force a full renderer reset (same backing `UIView`).
    var sessionKey: Int
    /// Only start bootstrap / run the display link when the Avatar tab is selected.
    var avatarTabVisible: Bool

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeUIView(context: Context) -> UIView {
        let v = JarvisBevyBackingView(frame: .zero)
        v.touchSink = context.coordinator
        return v
    }

    func updateUIView(_ uiView: UIView, context: Context) {
        let prev = context.coordinator.lastSessionKey
        if prev != sessionKey {
            if prev != nil {
                context.coordinator.teardown()
                JarvisIOSLog.recordBevy("sessionKey \(prev!) → \(sessionKey): coordinator reset (same UIView)")
            }
            context.coordinator.lastSessionKey = sessionKey
        }
        if let v = uiView as? JarvisBevyBackingView {
            v.touchSink = context.coordinator
        }
        uiView.layoutIfNeeded()
        context.coordinator.update(host: uiView, avatarTabVisible: avatarTabVisible)
    }

    static func dismantleUIView(_ uiView: UIView, coordinator: Coordinator) {
        coordinator.teardown()
    }

    @MainActor
    final class Coordinator: NSObject, JarvisBevyTouchSink {
        func jarvisDeliverTouch(phase: UInt8, x: CGFloat, y: CGFloat, id: UInt64) {
            guard let r = renderer else { return }
            jarvis_renderer_touch(r, phase, Float(x), Float(y), id)
        }

        fileprivate var lastSessionKey: Int?
        /// Weak: skip `jarvis_renderer_render` if the view left the hierarchy (background / container).
        private weak var renderingHost: UIView?
        private var renderer: UnsafeMutablePointer<UInt8>?
        private var displayLink: CADisplayLink?
        private var lastWidth: UInt32 = 0
        private var lastHeight: UInt32 = 0
        private var bootstrapStarted = false
        private var bootstrapTask: Task<Void, Never>?
        private var undersizedBoundsLogBudget = 8

        /// SwiftUI sometimes reports `contentScaleFactor == 1` on device; always prefer the screen scale for Bevy.
        private func displayScale(for host: UIView) -> Float {
            let a = host.contentScaleFactor
            let b = host.window?.screen.scale ?? UIScreen.main.scale
            return Float(Swift.max(a, b, 1.0))
        }

        @objc private func tick(_ link: CADisplayLink) {
            guard let r = renderer else { return }
            if renderingHost?.window == nil {
                return
            }
            jarvis_renderer_render(r, link.timestamp)
        }

        /// While About/Logs is shown: cancel in-flight bootstrap, stop the display link; keep Rust renderer if already created.
        private func pauseWhenTabNotAvatar() {
            bootstrapTask?.cancel()
            bootstrapTask = nil
            if renderer == nil {
                bootstrapStarted = false
            }
            if displayLink != nil {
                displayLink?.invalidate()
                displayLink = nil
                JarvisIOSLog.recordBevy("pause: Avatar tab background — display link stopped (Metal idle)")
            }
        }

        private func resumeDisplayLinkIfNeeded(host: UIView) {
            guard renderer != nil, displayLink == nil else { return }
            DispatchQueue.main.async { [self, host] in
                guard self.renderer != nil, self.displayLink == nil else { return }
                guard host.window != nil else {
                    JarvisIOSLog.recordBevy("resume: defer display link — window nil")
                    return
                }
                let link = CADisplayLink(target: self, selector: #selector(tick(_:)))
                link.add(to: .main, forMode: .common)
                self.displayLink = link
                JarvisIOSLog.recordBevy("resume: Avatar tab foreground — display link restarted (deferred)")
            }
        }

        func update(host: UIView, avatarTabVisible: Bool) {
            renderingHost = host
            guard avatarTabVisible else {
                pauseWhenTabNotAvatar()
                return
            }

            resumeDisplayLinkIfNeeded(host: host)

            let b = host.bounds
            if b.width <= 0 || b.height <= 0 {
                if undersizedBoundsLogBudget > 0 {
                    undersizedBoundsLogBudget -= 1
                    JarvisIOSLog.recordBevy(
                        "bootstrap: UIView bounds are zero w=\(b.width) h=\(b.height) super=\(String(describing: host.superview?.bounds)) — waiting for layout (GeometryReader frame); avoid `.id()` on this representable"
                    )
                }
                return
            }
            undersizedBoundsLogBudget = 8

            let scale = displayScale(for: host)
            let w = UInt32(max(1, (b.width * CGFloat(scale)).rounded(.toNearestOrAwayFromZero)))
            let h = UInt32(max(1, (b.height * CGFloat(scale)).rounded(.toNearestOrAwayFromZero)))

            if renderer == nil {
                if !bootstrapStarted {
                    bootstrapStarted = true
                    let hub = UserDefaults.standard.string(forKey: HubProfileSync.userDefaultsBaseURLKey)?
                        .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                    if hub.isEmpty {
                        JarvisIOSLog.recordBevy("bootstrap: no hub URL → bundled assets + immediate renderer")
                        HubProfileSync.prepareBundledOnlySync()
                        JarvisIOSLog.logJarvisEnv(JarvisIOSLog.bevy, tag: "bootstrap bundled")
                        startRenderer(host: host, w: w, h: h, scale: scale)
                    } else {
                        JarvisIOSLog.recordBevy("bootstrap: hub URL set → async prepareForBevyBootstrap then renderer w=\(w) h=\(h)")
                        bootstrapTask?.cancel()
                        bootstrapTask = Task { @MainActor in
                            await HubProfileSync.prepareForBevyBootstrap()
                            guard !Task.isCancelled else {
                                JarvisIOSLog.recordBevy("bootstrap: Task cancelled before startRenderer (tab/session change?)")
                                self.bootstrapStarted = false
                                return
                            }
                            JarvisIOSLog.logJarvisEnv(JarvisIOSLog.bevy, tag: "bootstrap after prepareForBevyBootstrap")
                            // Layout can pass 0×0 on an early pass then a real size after async work; always use current bounds.
                            host.layoutIfNeeded()
                            let b2 = host.bounds
                            let scale2 = self.displayScale(for: host)
                            guard b2.width > 0, b2.height > 0 else {
                                JarvisIOSLog.recordBevy(
                                    "bootstrap: defer startRenderer — bounds still zero after prepare (w=\(b2.width) h=\(b2.height)); will retry on next layout"
                                )
                                self.bootstrapStarted = false
                                return
                            }
                            // Match the no-hub path: use displayScale (max of view vs screen), not raw
                            // `contentScaleFactor` — SwiftUI often leaves the latter at 1 on device while
                            // Retina scale is 3, which would pass logical px as "physical" and break MSAA/swapchain.
                            let w2 = UInt32(max(1, (b2.width * CGFloat(scale2)).rounded(.toNearestOrAwayFromZero)))
                            let h2 = UInt32(max(1, (b2.height * CGFloat(scale2)).rounded(.toNearestOrAwayFromZero)))
                            self.startRenderer(host: host, w: w2, h: h2, scale: scale2)
                        }
                    }
                }
                return
            }

            if w != lastWidth || h != lastHeight {
                jarvis_renderer_resize(renderer!, w, h)
                lastWidth = w
                lastHeight = h
            }
        }

        func teardown() {
            renderingHost = nil
            bootstrapTask?.cancel()
            bootstrapTask = nil
            displayLink?.invalidate()
            displayLink = nil
            if let r = renderer {
                jarvis_renderer_free(r)
                renderer = nil
                JarvisBevySession.unregisterRenderer()
            }
            lastWidth = 0
            lastHeight = 0
            bootstrapStarted = false
            undersizedBoundsLogBudget = 8
        }

        private func startRenderer(host: UIView, w: UInt32, h: UInt32, scale: Float) {
            guard renderer == nil else { return }
            // LiveContainer / SwiftUI can attach the representable one frame before `window` is set.
            // Starting Metal + CADisplayLink without a window correlates with immediate SIGABRT in render.
            guard host.window != nil else {
                JarvisIOSLog.recordBevy(
                    "startRenderer: defer — UIView.window is nil (retry on next layout; common when opening Avatar tab)"
                )
                bootstrapStarted = false
                return
            }
            HubProfileSync.applyIosAvatarOverrideEnvFromUserDefaults()
            JarvisIOSLog.recordBevy("startRenderer: calling jarvis_renderer_new w=\(w) h=\(h) scale=\(scale)")
            let raw = Unmanaged.passUnretained(host).toOpaque()
            let ptr = raw.assumingMemoryBound(to: UInt8.self)
            let r = jarvis_renderer_new(ptr, w, h, scale)
            guard UnsafeRawPointer(r) != UnsafeRawPointer(bitPattern: 0) else {
                JarvisIOSLog.recordBevyError("startRenderer: jarvis_renderer_new returned NULL (see Rust stderr / eprintln)")
                bootstrapStarted = false
                return
            }
            JarvisIOSLog.recordBevy("startRenderer: jarvis_renderer_new OK (CADisplayLink deferred to next main run loop)")
            renderer = r
            JarvisBevySession.registerRenderer(r)
            lastWidth = w
            lastHeight = h
            DispatchQueue.main.async { [self, host] in
                guard self.renderer != nil, self.displayLink == nil else { return }
                guard host.window != nil else {
                    JarvisIOSLog.recordBevyError(
                        "startRenderer: window became nil before CADisplayLink — Metal not started"
                    )
                    return
                }
                let link = CADisplayLink(target: self, selector: #selector(tick(_:)))
                link.add(to: .main, forMode: .common)
                self.displayLink = link
                JarvisIOSLog.recordBevy("startRenderer: CADisplayLink attached")
            }
        }
    }
}
