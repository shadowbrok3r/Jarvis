import AVFoundation
import Foundation

/// Outcome of configuring `AVAudioSession` for debug mic metering / future capture.
enum JarvisAudioSessionOutcome: Equatable, Sendable {
    case ok
    case failure(String)

    var errorMessage: String? {
        switch self {
        case .ok: nil
        case .failure(let s): s
        }
    }
}

/// Configures `AVAudioSession.sharedInstance()` for `.playAndRecord` with speaker-friendly routing.
///
/// Uses `.defaultToSpeaker` so monitoring and playback route to the built-in speaker instead of the
/// earpiece receiver (typical for “hold at arm’s length” / debug UIs). Full capture/streaming is out of scope.
@MainActor
enum JarvisAudioSession {
    private static let session = AVAudioSession.sharedInstance()

    /// Sets category/mode/options and activates the session for input + output.
    static func configureAndActivate() -> JarvisAudioSessionOutcome {
        do {
            try session.setCategory(
                .playAndRecord,
                mode: .default,
                options: [.defaultToSpeaker, .allowBluetooth]
            )
            try session.setActive(true, options: [])
            return .ok
        } catch {
            return .failure(error.localizedDescription)
        }
    }

    /// Deactivates the session and notifies other audio (e.g. Music) that it may resume.
    static func deactivate() -> JarvisAudioSessionOutcome {
        do {
            try session.setActive(false, options: [.notifyOthersOnDeactivation])
            return .ok
        } catch {
            return .failure(error.localizedDescription)
        }
    }
}
