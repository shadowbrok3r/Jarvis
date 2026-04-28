import BridgeFFI
import Darwin
import Foundation
import os

// MARK: - In-app log bus (Swift + polled Rust buffer)

/// Ring buffer shown on the **Logs** tab. Thread-safe append from any isolation.
@MainActor
@Observable
final class JarvisIOSLogBus {
    static let shared = JarvisIOSLogBus()

    private(set) var swiftLines: [String] = []
    private(set) var rustText: String = ""

    private let maxSwiftLines = 600
    private let dateFormatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    /// Single scrollable body: Swift events first, then Rust buffer from `jarvis_ios_debug_log_snapshot()`.
    var displayText: String {
        let swift = swiftLines.joined(separator: "\n")
        if swift.isEmpty { return rustText.isEmpty ? "(no log lines yet)" : "--- Rust ---\n" + rustText }
        if rustText.isEmpty { return "--- Swift ---\n" + swift }
        return "--- Swift ---\n" + swift + "\n\n--- Rust ---\n" + rustText
    }

    func appendSwiftLine(_ line: String) {
        let ts = dateFormatter.string(from: Date())
        swiftLines.append("\(ts) \(line)")
        if swiftLines.count > maxSwiftLines {
            swiftLines.removeFirst(swiftLines.count - maxSwiftLines)
        }
    }

    func setRustSnapshot(_ text: String) {
        rustText = text
    }

    func clearAll() {
        swiftLines.removeAll(keepingCapacity: true)
        rustText = ""
        jarvis_ios_debug_log_clear()
    }

    /// Call from nonisolated contexts (URLSession, etc.).
    nonisolated static func appendSwiftLine(_ line: String) {
        Task { @MainActor in
            JarvisIOSLogBus.shared.appendSwiftLine(line)
        }
    }
}

// MARK: - Unified subsystem for optional OSLog (Console.app)

enum JarvisIOSLog {
    private static let subsystem = "JarvisIOS"

    static let hub = Logger(subsystem: subsystem, category: "HubProfile")
    static let bevy = Logger(subsystem: subsystem, category: "BevyUIView")
    static let ui = Logger(subsystem: subsystem, category: "MainShell")

    static func getenvString(_ name: String) -> String? {
        name.withCString { cName in
            guard let ptr = getenv(cName) else { return nil }
            return String(cString: ptr)
        }
    }

    static func logJarvisEnv(_ log: Logger, tag: String) {
        let ar = getenvString("JARVIS_ASSET_ROOT") ?? "(unset)"
        let mp = getenvString("JARVIS_PROFILE_MANIFEST") ?? "(unset)"
        let msg = "\(tag) JARVIS_ASSET_ROOT=\(ar) JARVIS_PROFILE_MANIFEST=\(mp)"
        JarvisIOSLogBus.appendSwiftLine("[Swift] \(msg)")
        log.info("\(msg, privacy: .public)")
    }

    // MARK: Hub

    static func recordHub(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Hub] \(message)")
        hub.info("\(message, privacy: .public)")
    }

    static func recordHubError(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Hub][error] \(message)")
        hub.error("\(message, privacy: .public)")
    }

    static func recordHubWarning(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Hub][warn] \(message)")
        hub.warning("\(message, privacy: .public)")
    }

    static func recordHubDebug(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Hub][debug] \(message)")
        hub.debug("\(message, privacy: .public)")
    }

    // MARK: Bevy host

    static func recordBevy(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Bevy] \(message)")
        bevy.info("\(message, privacy: .public)")
    }

    static func recordBevyError(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Bevy][error] \(message)")
        bevy.error("\(message, privacy: .public)")
    }

    // MARK: UI

    static func recordUI(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][UI] \(message)")
        ui.info("\(message, privacy: .public)")
    }

    static func recordUIError(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][UI][error] \(message)")
        ui.error("\(message, privacy: .public)")
    }
}
