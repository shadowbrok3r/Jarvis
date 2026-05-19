import CoreMotion
import Foundation
import Observation

/// Which VRMC spring joints receive phone tilt / shake.
enum DeviceMotionSpringScope: String, CaseIterable, Identifiable {
    /// Every spring joint on the VRM (matches desktop spring-bone physics).
    case allSprings = "all"
    /// Hair, cloth, accessories — skips humanoid core name tokens.
    case hairClothOnly = "secondary"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .allSprings: return "All spring bones"
        case .hairClothOnly: return "Hair / cloth only"
        }
    }

    var rustCode: UInt8 {
        switch self {
        case .allSprings: return 0
        case .hairClothOnly: return 1
        }
    }

    static func fromStored(_ raw: String) -> Self {
        Self(rawValue: raw) ?? .allSprings
    }
}

/// Streams device gravity + user acceleration into Rust spring bones (VRMC secondary motion only).
@MainActor
@Observable
final class JarvisDeviceMotion {
    static let shared = JarvisDeviceMotion()

    /// Live gravity direction in Bevy space (Y-up), normalized.
    private(set) var gravityDisplay = SIMD3<Double>(0, -1, 0)
    /// User acceleration in Bevy space (m/s²).
    private(set) var accelDisplay = SIMD3<Double>(0, 0, 0)
    /// Horizontal tilt magnitude in degrees (0 = flat / level).
    private(set) var tiltDegrees: Double = 0
    /// Shake magnitude in m/s² (after deadzone).
    private(set) var shakeMagnitude: Double = 0

    /// When false, Rust restores per-joint gravity from the VRM / spring preset.
    var enabled: Bool {
        get { UserDefaults.standard.object(forKey: Keys.enabled) as? Bool ?? false }
        set { UserDefaults.standard.set(newValue, forKey: Keys.enabled) }
    }

    /// EMA alpha for gravity smoothing (0.05–0.5).
    var gravitySmoothing: Double {
        get { storedDouble(Keys.gravitySmooth, default: 0.14) }
        set { UserDefaults.standard.set(clamp(newValue, 0.05, 0.5), forKey: Keys.gravitySmooth) }
    }

    /// EMA alpha for acceleration smoothing (0.05–0.5).
    var accelSmoothing: Double {
        get { storedDouble(Keys.accelSmooth, default: 0.22) }
        set { UserDefaults.standard.set(clamp(newValue, 0.05, 0.5), forKey: Keys.accelSmooth) }
    }

    /// How strongly phone tilt steers spring gravity (0–1).
    var gravityBlend: Double {
        get { storedDouble(Keys.gravityBlend, default: 0.72) }
        set { UserDefaults.standard.set(clamp(newValue, 0, 1), forKey: Keys.gravityBlend) }
    }

    /// Max spring gravity tilt from world down (degrees).
    var maxTiltDegrees: Double {
        get { storedDouble(Keys.maxTiltDeg, default: 66) }
        set { UserDefaults.standard.set(clamp(newValue, 5, 85), forKey: Keys.maxTiltDeg) }
    }

    /// Extra spring power per m/s² of shake.
    var shakePower: Double {
        get { storedDouble(Keys.shakePower, default: 0.18) }
        set { UserDefaults.standard.set(clamp(newValue, 0, 1), forKey: Keys.shakePower) }
    }

    /// Cap on shake power multiplier.
    var maxShakeMultiplier: Double {
        get { storedDouble(Keys.maxShakeMult, default: 3) }
        set { UserDefaults.standard.set(clamp(newValue, 1, 8), forKey: Keys.maxShakeMult) }
    }

    /// Ignore shake below this m/s².
    var shakeDeadzone: Double {
        get { storedDouble(Keys.shakeDeadzone, default: 0.12) }
        set { UserDefaults.standard.set(clamp(newValue, 0, 1), forKey: Keys.shakeDeadzone) }
    }

    /// Multiplies VRMC spring gravity power (0 = limp, 1 = default, up to 3).
    var springGravityScale: Double {
        get { storedDouble(Keys.springGravityScale, default: 1) }
        set { UserDefaults.standard.set(clamp(newValue, 0, 3), forKey: Keys.springGravityScale) }
    }

    /// Multiplies VRMC spring drag (higher = less sway).
    var springDragScale: Double {
        get { storedDouble(Keys.springDragScale, default: 1) }
        set { UserDefaults.standard.set(clamp(newValue, 0.05, 5), forKey: Keys.springDragScale) }
    }

    /// Which spring joints phone motion steers (see `DeviceMotionSpringScope`).
    var springScope: DeviceMotionSpringScope {
        get {
            let raw = UserDefaults.standard.string(forKey: Keys.springScope) ?? DeviceMotionSpringScope.allSprings.rawValue
            return DeviceMotionSpringScope.fromStored(raw)
        }
        set { UserDefaults.standard.set(newValue.rawValue, forKey: Keys.springScope) }
    }

    private enum Keys {
        static let enabled = "jarvis.ios.phoneSpringGravity"
        static let springScope = "jarvis.ios.motion.springScope"
        static let gravitySmooth = "jarvis.ios.motion.gravitySmooth"
        static let accelSmooth = "jarvis.ios.motion.accelSmooth"
        static let gravityBlend = "jarvis.ios.motion.gravityBlend"
        static let maxTiltDeg = "jarvis.ios.motion.maxTiltDeg"
        static let shakePower = "jarvis.ios.motion.shakePower"
        static let maxShakeMult = "jarvis.ios.motion.maxShakeMult"
        static let shakeDeadzone = "jarvis.ios.motion.shakeDeadzone"
        static let springGravityScale = "jarvis.ios.motion.springGravityScale"
        static let springDragScale = "jarvis.ios.motion.springDragScale"
    }

    private let manager = CMMotionManager()
    private var gravityBevy = SIMD3<Double>(0, -1, 0)
    private var accelBevy = SIMD3<Double>(0, 0, 0)

    private init() {}

    func start() {
        guard manager.isDeviceMotionAvailable else {
            JarvisIOSLog.recordBevy("device motion: unavailable on this device")
            return
        }
        guard !manager.isDeviceMotionActive else { return }
        manager.deviceMotionUpdateInterval = 1.0 / 60.0
        manager.startDeviceMotionUpdates(to: .main) { [weak self] motion, err in
            guard let self, let motion else {
                if let err {
                    JarvisIOSLog.recordBevy("device motion error: \(err.localizedDescription)")
                }
                return
            }
            let gAlpha = self.gravitySmoothing
            let aAlpha = self.accelSmoothing
            let rawG = Self.referenceToBevy(motion.gravity)
            let rawA = Self.rawAccelToBevy(motion.userAcceleration)
            self.gravityBevy = Self.smooth(self.gravityBevy, toward: rawG, alpha: gAlpha)
            self.accelBevy = Self.smooth(self.accelBevy, toward: rawA, alpha: aAlpha)
            // Never feed upward gravity into springs (180° flip source when sensor noise crosses zero).
            if self.gravityBevy.y > 0 {
                self.gravityBevy = -self.gravityBevy
            }

            self.gravityDisplay = self.gravityBevy
            self.accelDisplay = self.accelBevy
            let horiz = sqrt(self.gravityBevy.x * self.gravityBevy.x + self.gravityBevy.z * self.gravityBevy.z)
            self.tiltDegrees = min(90, asin(min(1, horiz)) * 180 / .pi)
            let rawShake = sqrt(
                self.accelBevy.x * self.accelBevy.x
                    + self.accelBevy.y * self.accelBevy.y
                    + self.accelBevy.z * self.accelBevy.z
            )
            self.shakeMagnitude = max(0, rawShake - self.shakeDeadzone)
        }
        JarvisIOSLog.recordBevy("device motion: started")
    }

    func stop() {
        guard manager.isDeviceMotionActive else { return }
        manager.stopDeviceMotionUpdates()
        accelBevy = .zero
        accelDisplay = .zero
        shakeMagnitude = 0
        JarvisIOSLog.recordBevy("device motion: stopped")
    }

    func pushToRenderer(_ ptr: UnsafeMutablePointer<UInt8>) {
        if !enabled {
            jarvis_renderer_set_device_motion(ptr, 0, -1, 0, 0, 0, 0, 0)
            return
        }
        let g = gravityBevy
        let a = accelBevy
        jarvis_renderer_set_device_motion(
            ptr,
            Float(g.x), Float(g.y), Float(g.z),
            Float(a.x), Float(a.y), Float(a.z),
            1
        )
    }

    /// Push Rust spring tuning from persisted UserDefaults.
    func pushTuningToRenderer(_ ptr: UnsafeMutablePointer<UInt8>) {
        jarvis_renderer_set_device_motion_tuning(
            ptr,
            Float(gravityBlend),
            Float(maxTiltDegrees),
            Float(shakePower),
            Float(maxShakeMultiplier),
            Float(shakeDeadzone),
            springScope.rustCode,
            Float(springGravityScale),
            Float(springDragScale)
        )
    }

    func resetTuningToDefaults() {
        gravitySmoothing = 0.14
        accelSmoothing = 0.22
        gravityBlend = 0.72
        maxTiltDegrees = 66
        shakePower = 0.18
        maxShakeMultiplier = 3
        shakeDeadzone = 0.12
        springGravityScale = 1
        springDragScale = 1
        springScope = .allSprings
        JarvisBevySession.pushDeviceMotionTuning()
    }

    /// CoreMotion reference frame (Z vertical) → Bevy Y-up unit gravity direction.
    private static func referenceToBevy(_ v: CMAcceleration) -> SIMD3<Double> {
        let gx = v.x
        let gy = v.z
        let gz = v.y
        let len = sqrt(gx * gx + gy * gy + gz * gz)
        guard len > 1e-6 else { return SIMD3(0, -1, 0) }
        return SIMD3(gx / len, gy / len, gz / len)
    }

    /// User acceleration in Bevy Y-up (not normalized).
    private static func rawAccelToBevy(_ v: CMAcceleration) -> SIMD3<Double> {
        SIMD3(v.x, v.z, v.y)
    }

    private static func smooth(_ current: SIMD3<Double>, toward target: SIMD3<Double>, alpha: Double) -> SIMD3<Double> {
        current + (target - current) * alpha
    }

    private func storedDouble(_ key: String, default defaultValue: Double) -> Double {
        if UserDefaults.standard.object(forKey: key) == nil { return defaultValue }
        return UserDefaults.standard.double(forKey: key)
    }

    private func clamp(_ v: Double, _ lo: Double, _ hi: Double) -> Double {
        min(max(v, lo), hi)
    }
}
