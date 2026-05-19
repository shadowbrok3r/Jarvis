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

/// How CoreMotion orientation maps to Bevy spring pull direction (Y-up, avatar faces -Z).
enum DeviceMotionGravityMode: String, CaseIterable, Identifiable {
    /// Portrait screen bottom points "down" for springs (upside-down phone → hair up).
    case screenBottom = "screen_bottom"
    /// Portrait screen top points "down" for springs.
    case screenTop = "screen_top"
    /// Real-world gravity from the sensor (earth down; phone flip does not invert hair).
    case worldGravity = "world_gravity"
    /// Screen normal (out of display toward you in portrait).
    case screenNormal = "screen_normal"
    /// Legacy axis remap of reference gravity `(x, z, y)` — no attitude.
    case legacyAxis = "legacy_axis"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .screenBottom: return "Screen bottom"
        case .screenTop: return "Screen top"
        case .worldGravity: return "World gravity"
        case .screenNormal: return "Screen normal"
        case .legacyAxis: return "Legacy axes"
        }
    }

    var detail: String {
        switch self {
        case .screenBottom:
            return "Pull follows the bottom edge of the screen. Best for upside-down hair flip."
        case .screenTop:
            return "Pull follows the top edge (port toward ceiling when upright)."
        case .worldGravity:
            return "Pull follows real gravity. Upright phone ≈ avatar down regardless of roll."
        case .screenNormal:
            return "Pull follows the direction out of the screen (toward your face in portrait)."
        case .legacyAxis:
            return "Raw reference-frame gravity with fixed axis remap (older behavior)."
        }
    }

    static func fromStored(_ raw: String) -> Self {
        Self(rawValue: raw) ?? .screenBottom
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
        get { storedDouble(Keys.accelSmooth, default: 0.35) }
        set { UserDefaults.standard.set(clamp(newValue, 0.05, 0.5), forKey: Keys.accelSmooth) }
    }

    /// How strongly phone tilt steers spring gravity (0–1).
    var gravityBlend: Double {
        get { storedDouble(Keys.gravityBlend, default: 0.72) }
        set { UserDefaults.standard.set(clamp(newValue, 0, 1), forKey: Keys.gravityBlend) }
    }

    /// Max spring gravity tilt from world down (degrees). 180 = full sphere (upside-down phone).
    var maxTiltDegrees: Double {
        get { storedDouble(Keys.maxTiltDeg, default: 180) }
        set { UserDefaults.standard.set(clamp(newValue, 5, 180), forKey: Keys.maxTiltDeg) }
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
        get { storedDouble(Keys.shakeDeadzone, default: 0.05) }
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

    /// How phone orientation maps to spring pull direction.
    var gravityMode: DeviceMotionGravityMode {
        get {
            let raw = UserDefaults.standard.string(forKey: Keys.gravityMode) ?? DeviceMotionGravityMode.screenBottom.rawValue
            return DeviceMotionGravityMode.fromStored(raw)
        }
        set { UserDefaults.standard.set(newValue.rawValue, forKey: Keys.gravityMode) }
    }

    /// Negate the computed pull vector (flip spring gravity 180°).
    var invertGravityPull: Bool {
        get { UserDefaults.standard.object(forKey: Keys.invertGravityPull) as? Bool ?? false }
        set { UserDefaults.standard.set(newValue, forKey: Keys.invertGravityPull) }
    }

    private enum Keys {
        static let enabled = "jarvis.ios.phoneSpringGravity"
        static let springScope = "jarvis.ios.motion.springScope"
        static let gravityMode = "jarvis.ios.motion.gravityMode"
        static let invertGravityPull = "jarvis.ios.motion.invertGravityPull"
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
    private var rawAccelBevy = SIMD3<Double>(0, 0, 0)

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
            let rawG = Self.pullDirectionToBevy(motion, mode: self.gravityMode, invert: self.invertGravityPull)
            let rawA = Self.rawAccelToBevy(motion.userAcceleration)
            self.rawAccelBevy = rawA
            self.gravityBevy = Self.smooth(self.gravityBevy, toward: rawG, alpha: gAlpha)
            self.accelBevy = Self.smooth(self.accelBevy, toward: rawA, alpha: aAlpha)

            self.gravityDisplay = self.gravityBevy
            self.accelDisplay = self.accelBevy
            let fromDown = min(max(-self.gravityBevy.y, -1), 1)
            self.tiltDegrees = acos(fromDown) * 180 / .pi
            let rawShake = sqrt(
                rawA.x * rawA.x + rawA.y * rawA.y + rawA.z * rawA.z
            )
            let smoothedShake = sqrt(
                self.accelBevy.x * self.accelBevy.x
                    + self.accelBevy.y * self.accelBevy.y
                    + self.accelBevy.z * self.accelBevy.z
            )
            // Peak-biased: raw spikes from shakes survive EMA damping for UI + Rust.
            let responsiveShake = max(rawShake, smoothedShake)
            self.shakeMagnitude = max(0, responsiveShake - self.shakeDeadzone)
        }
        JarvisIOSLog.recordBevy("device motion: started")
    }

    func stop() {
        guard manager.isDeviceMotionActive else { return }
        manager.stopDeviceMotionUpdates()
        accelBevy = .zero
        rawAccelBevy = .zero
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
        // Push raw user accel so Rust shake boost responds to fast jolts, not laggy EMA.
        let a = rawAccelBevy
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
        accelSmoothing = 0.35
        gravityBlend = 0.72
        maxTiltDegrees = 180
        shakePower = 0.18
        maxShakeMultiplier = 3
        shakeDeadzone = 0.05
        springGravityScale = 1
        springDragScale = 1
        springScope = .allSprings
        gravityMode = .screenBottom
        invertGravityPull = false
        JarvisBevySession.pushDeviceMotionTuning()
    }

    private static func pullDirectionToBevy(
        _ motion: CMDeviceMotion,
        mode: DeviceMotionGravityMode,
        invert: Bool
    ) -> SIMD3<Double> {
        let v: SIMD3<Double> = switch mode {
        case .screenBottom:
            deviceAxisPullToBevy(motion, deviceAxis: SIMD3(0, -1, 0))
        case .screenTop:
            deviceAxisPullToBevy(motion, deviceAxis: SIMD3(0, 1, 0))
        case .worldGravity:
            bevyFromReference(SIMD3(motion.gravity.x, motion.gravity.y, motion.gravity.z))
        case .screenNormal:
            deviceAxisPullToBevy(motion, deviceAxis: SIMD3(0, 0, 1))
        case .legacyAxis:
            legacyGravityToBevy(motion.gravity)
        }
        return invert ? -v : v
    }

    /// Reference-frame gravity with the pre-attitude `(x, z, y)` remap.
    private static func legacyGravityToBevy(_ g: CMAcceleration) -> SIMD3<Double> {
        let len = sqrt(g.x * g.x + g.y * g.y + g.z * g.z)
        guard len > 1e-6 else { return SIMD3(0, -1, 0) }
        return SIMD3(g.x / len, g.z / len, g.y / len)
    }

    /// Map a unit vector fixed in device space (portrait) to Bevy pull direction.
    private static func deviceAxisPullToBevy(_ motion: CMDeviceMotion, deviceAxis: SIMD3<Double>) -> SIMD3<Double> {
        let vRef = referenceVector(fromDevice: deviceAxis, motion: motion)
        return bevyFromReference(vRef)
    }

    /// `rotationMatrix` maps reference → device; transpose maps device → reference.
    private static func referenceVector(fromDevice device: SIMD3<Double>, motion: CMDeviceMotion) -> SIMD3<Double> {
        let m = motion.attitude.rotationMatrix
        return SIMD3(
            m.m11 * device.x + m.m21 * device.y + m.m31 * device.z,
            m.m12 * device.x + m.m22 * device.y + m.m32 * device.z,
            m.m13 * device.x + m.m23 * device.y + m.m33 * device.z
        )
    }

    /// CoreMotion reference (Z vertical) → Bevy (Y up, avatar faces -Z).
    private static func bevyFromReference(_ v: SIMD3<Double>) -> SIMD3<Double> {
        let len = sqrt(v.x * v.x + v.y * v.y + v.z * v.z)
        guard len > 1e-6 else { return SIMD3(0, -1, 0) }
        return SIMD3(v.x / len, v.z / len, -v.y / len)
    }

    /// User acceleration in reference frame → Bevy Y-up (magnitude preserved).
    private static func rawAccelToBevy(_ v: CMAcceleration) -> SIMD3<Double> {
        SIMD3(v.x, v.z, -v.y)
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
