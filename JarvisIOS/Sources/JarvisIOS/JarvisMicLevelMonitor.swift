import AVFoundation
import Foundation

/// Thread-safe RMS smoothing state updated from the audio tap’s processing queue.
private final class MicLevelSmoothing: @unchecked Sendable {
    private let lock = NSLock()
    private var smoothed: Float = 0

    func update(rms: Float, alpha: Float) -> Float {
        lock.lock()
        defer { lock.unlock() }
        smoothed = smoothed * (1 - alpha) + rms * alpha
        return smoothed
    }

    func reset() {
        lock.lock()
        smoothed = 0
        lock.unlock()
    }
}

/// Live RMS-based level (0…1) from the built-in mic via `AVAudioEngine` input tap; for **Logs** debug UI only.
@MainActor
@Observable
final class JarvisMicLevelMonitor {
    private(set) var normalizedLevel: Double = 0
    private(set) var isRunning = false
    private(set) var lastSessionOutcome: JarvisAudioSessionOutcome?
    private(set) var lastError: String?

    private let engine = AVAudioEngine()
    nonisolated private let processQueue = DispatchQueue(
        label: "ai.jarvis.JarvisMicLevelMonitor.process",
        qos: .userInitiated
    )
    nonisolated private let smoothing = MicLevelSmoothing()
    nonisolated private let smoothAlpha: Float = 0.25

    /// Idempotent: configures and activates `AVAudioSession` (same path as the start of `startMeter()`).
    func prepareSessionOnly() {
        lastError = nil
        let outcome = JarvisAudioSession.configureAndActivate()
        lastSessionOutcome = outcome
        if case .failure(let msg) = outcome {
            lastError = msg
            JarvisIOSLog.recordAudioError("prepareSessionOnly: \(msg)")
        } else {
            JarvisIOSLog.recordAudio("prepareSessionOnly ok")
        }
    }

    /// Activates the audio session, installs the input tap, and starts the engine.
    func startMeter() {
        lastError = nil
        let outcome = JarvisAudioSession.configureAndActivate()
        lastSessionOutcome = outcome
        guard case .ok = outcome else {
            if case .failure(let msg) = outcome {
                lastError = msg
                JarvisIOSLog.recordAudioError("startMeter: session \(msg)")
            }
            return
        }

        guard !engine.isRunning else {
            JarvisIOSLog.recordAudio("startMeter: engine already running")
            return
        }

        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        let bufferSize: AVAudioFrameCount = 1024

        engine.reset()
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: bufferSize, format: format) { [weak self] buffer, _ in
            guard let self else { return }
            let rms = Self.rmsMono(from: buffer)
            self.processQueue.async {
                let smoothed = self.smoothing.update(rms: rms, alpha: self.smoothAlpha)
                let normalized = Self.normalizeRmsToUnitInterval(smoothed)
                Task { @MainActor in
                    self.normalizedLevel = normalized
                }
            }
        }

        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            let msg = error.localizedDescription
            lastError = msg
            JarvisIOSLog.recordAudioError("startMeter: engine.start \(msg)")
            return
        }

        isRunning = true
        JarvisIOSLog.recordAudio(
            "startMeter ok (format \(format.sampleRate) Hz, \(format.channelCount) ch)"
        )
    }

    func stopMeter() {
        engine.inputNode.removeTap(onBus: 0)
        if engine.isRunning {
            engine.stop()
        }
        processQueue.sync {
            smoothing.reset()
        }
        normalizedLevel = 0
        isRunning = false

        let deact = JarvisAudioSession.deactivate()
        lastSessionOutcome = deact
        if case .failure(let msg) = deact {
            lastError = msg
            JarvisIOSLog.recordAudioError("stopMeter: deactivate \(msg)")
        } else {
            JarvisIOSLog.recordAudio("stopMeter: engine stopped, session deactivated")
        }
    }

    nonisolated private static func rmsMono(from buffer: AVAudioPCMBuffer) -> Float {
        let frames = Int(buffer.frameLength)
        guard frames > 0 else { return 0 }
        let chCount = Int(buffer.format.channelCount)
        guard chCount > 0, let data = buffer.floatChannelData else { return 0 }

        var sumChannels: Float = 0
        for c in 0..<chCount {
            let ptr = data[c]
            var sum: Float = 0
            for i in 0..<frames {
                let s = ptr[i]
                sum += s * s
            }
            sumChannels += sqrt(sum / Float(frames))
        }
        return sumChannels / Float(chCount)
    }

    /// Maps RMS to 0…1 for `ProgressView` (roughly −55 dBFS → 0, 0 dBFS → 1).
    nonisolated private static func normalizeRmsToUnitInterval(_ rms: Float) -> Double {
        let floor: Float = 1e-5
        let clamped = max(rms, floor)
        let db = 20 * log10(clamped)
        let minDb: Float = -55
        let maxDb: Float = 0
        let t = (db - minDb) / (maxDb - minDb)
        return Double(min(1, max(0, t)))
    }
}
