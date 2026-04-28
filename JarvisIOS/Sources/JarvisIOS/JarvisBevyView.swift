import SwiftUI
import QuartzCore
import UIKit

/// Hosts a `UIView` for Bevy: Rust receives a UIKit `RawWindowHandle` and drives Metal via wgpu each frame.
struct JarvisBevyView: UIViewRepresentable {
    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeUIView(context: Context) -> BevyHostUIView {
        let coord = context.coordinator
        let v = BevyHostUIView()
        v.backgroundColor = .black
        v.onLayout = { [weak v] in
            guard let v else { return }
            coord.resizeIfNeeded(host: v)
        }
        return v
    }

    func updateUIView(_ uiView: BevyHostUIView, context: Context) {
        context.coordinator.ensureStarted(host: uiView)
        context.coordinator.resizeIfNeeded(host: uiView)
    }

    static func dismantleUIView(_ uiView: BevyHostUIView, coordinator: Coordinator) {
        coordinator.teardown()
    }

    @MainActor
    final class Coordinator: NSObject {
        private var renderer: UnsafeMutablePointer<UInt8>?
        private var link: CADisplayLink?
        private var t0 = CACurrentMediaTime()
        private var lastW: UInt32 = 0
        private var lastH: UInt32 = 0

        func ensureStarted(host: BevyHostUIView) {
            guard renderer == nil else { return }

            let scale = Float(host.contentScaleFactor)
            let wPx = UInt32(max(host.bounds.width * host.contentScaleFactor, 1))
            let hPx = UInt32(max(host.bounds.height * host.contentScaleFactor, 1))
            if wPx < 2 || hPx < 2 {
                DispatchQueue.main.async { [weak self, weak host] in
                    guard let self, let host else { return }
                    self.ensureStarted(host: host)
                }
                return
            }

            lastW = wPx
            lastH = hPx

            let raw = UnsafeMutableRawPointer(Unmanaged.passUnretained(host).toOpaque())
            let uiPtr = raw.assumingMemoryBound(to: UInt8.self)

            let ptr = jarvis_renderer_new(uiPtr, wPx, hPx, scale)
            guard Int(bitPattern: ptr) != 0 else { return }
            renderer = ptr

            let dl = CADisplayLink(target: self, selector: #selector(tick))
            dl.add(to: .main, forMode: .common)
            link = dl
        }

        func teardown() {
            link?.invalidate()
            link = nil
            if let r = renderer {
                jarvis_renderer_free(r)
                renderer = nil
            }
        }

        func resizeIfNeeded(host: BevyHostUIView) {
            guard let r = renderer else { return }
            let wPx = UInt32(max(host.bounds.width * host.contentScaleFactor, 1))
            let hPx = UInt32(max(host.bounds.height * host.contentScaleFactor, 1))
            if wPx == 0 || hPx == 0 { return }
            if wPx != lastW || hPx != lastH {
                lastW = wPx
                lastH = hPx
                jarvis_renderer_resize(r, wPx, hPx)
            }
        }

        @objc private func tick() {
            guard let r = renderer else { return }
            let t = CACurrentMediaTime() - t0
            jarvis_renderer_render(r, t)
        }
    }
}

final class BevyHostUIView: UIView {
    var onLayout: (() -> Void)?

    override func layoutSubviews() {
        super.layoutSubviews()
        onLayout?()
    }
}
