import Foundation
import QuartzCore
import SwiftUI
import UIKit

/// Solid backing view for Metal; plain `UIView` is fine once SwiftUI assigns a frame.
private final class JarvisBevyBackingView: UIView {
    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }
}

/// Hosts the Rust Bevy renderer in a `UIView` (Metal via wgpu); drives `jarvis_renderer_*` on the main thread.
///
/// Use with an explicit `.frame` from `GeometryReader` (see `MainShellView`): `TabView` often proposes 0×0
/// for `UIViewRepresentable`, which never starts `jarvis_renderer_new`.
@MainActor
struct JarvisBevyView: UIViewRepresentable {
    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeUIView(context: Context) -> UIView {
        JarvisBevyBackingView(frame: .zero)
    }

    func updateUIView(_ uiView: UIView, context: Context) {
        uiView.layoutIfNeeded()
        context.coordinator.update(host: uiView)
    }

    static func dismantleUIView(_ uiView: UIView, coordinator: Coordinator) {
        coordinator.teardown()
    }

    @MainActor
    final class Coordinator: NSObject {
        private var renderer: UnsafeMutablePointer<UInt8>?
        private var displayLink: CADisplayLink?
        private var lastWidth: UInt32 = 0
        private var lastHeight: UInt32 = 0
        private var bootstrapStarted = false
        private var bootstrapTask: Task<Void, Never>?
        private var undersizedBoundsLogBudget = 8

        @objc private func tick(_ link: CADisplayLink) {
            guard let r = renderer else { return }
            jarvis_renderer_render(r, link.timestamp)
        }

        func update(host: UIView) {
            let b = host.bounds
            if b.width <= 0 || b.height <= 0 {
                if undersizedBoundsLogBudget > 0 {
                    undersizedBoundsLogBudget -= 1
                    JarvisIOSLog.recordBevy(
                        "bootstrap: UIView bounds are zero w=\(b.width) h=\(b.height) super=\(String(describing: host.superview?.bounds)) — give JarvisBevyView a non‑zero SwiftUI frame (GeometryReader in MainShellView)"
                    )
                }
                return
            }
            undersizedBoundsLogBudget = 8

            let scale = Float(host.contentScaleFactor)
            let w = UInt32(max(1, (b.width * host.contentScaleFactor).rounded(.toNearestOrAwayFromZero)))
            let h = UInt32(max(1, (b.height * host.contentScaleFactor).rounded(.toNearestOrAwayFromZero)))

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
                        let wCopy = w
                        let hCopy = h
                        let scaleCopy = scale
                        bootstrapTask?.cancel()
                        bootstrapTask = Task { @MainActor in
                            await HubProfileSync.prepareForBevyBootstrap()
                            guard !Task.isCancelled else {
                                JarvisIOSLog.recordBevy("bootstrap: Task cancelled before startRenderer (tab/session change?)")
                                self.bootstrapStarted = false
                                return
                            }
                            JarvisIOSLog.logJarvisEnv(JarvisIOSLog.bevy, tag: "bootstrap after prepareForBevyBootstrap")
                            self.startRenderer(host: host, w: wCopy, h: hCopy, scale: scaleCopy)
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
            bootstrapTask?.cancel()
            bootstrapTask = nil
            displayLink?.invalidate()
            displayLink = nil
            if let r = renderer {
                jarvis_renderer_free(r)
                renderer = nil
            }
            lastWidth = 0
            lastHeight = 0
            bootstrapStarted = false
        }

        private func startRenderer(host: UIView, w: UInt32, h: UInt32, scale: Float) {
            guard renderer == nil else { return }
            JarvisIOSLog.recordBevy("startRenderer: calling jarvis_renderer_new w=\(w) h=\(h) scale=\(scale)")
            let raw = Unmanaged.passUnretained(host).toOpaque()
            let ptr = raw.assumingMemoryBound(to: UInt8.self)
            let r = jarvis_renderer_new(ptr, w, h, scale)
            guard UnsafeRawPointer(r) != UnsafeRawPointer(bitPattern: 0) else {
                JarvisIOSLog.recordBevyError("startRenderer: jarvis_renderer_new returned NULL (see Rust stderr / eprintln)")
                bootstrapStarted = false
                return
            }
            JarvisIOSLog.recordBevy("startRenderer: jarvis_renderer_new OK, display link started")
            renderer = r
            lastWidth = w
            lastHeight = h
            let link = CADisplayLink(target: self, selector: #selector(tick(_:)))
            link.add(to: .main, forMode: .common)
            displayLink = link
        }
    }
}
