import BridgeFFI
import Darwin
import Foundation
import os

// MARK: - Persistent crash log file

/// Manages the two log file paths and wires Swift + Rust to write there.
/// Call `JarvisIOSCrashLog.setup()` once at app start, before Bevy boots.
enum JarvisIOSCrashLog {
    static let sessionFileName  = "session_log.txt"
    static let previousFileName = "prev_session_log.txt"

    nonisolated(unsafe) private static var _sessionURL: URL?
    nonisolated(unsafe) private static var _fileHandle: FileHandle?
    private static let writeLock = NSLock()

    /// App-Support subdir for all Jarvis log files.
    static func logsDirectory() -> URL? {
        guard let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else {
            return nil
        }
        let dir = base.appendingPathComponent("JarvisIOSLogs", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// Path to the previous (crashed) session log.  Nil if it doesn't exist yet.
    static var previousSessionLogURL: URL? {
        guard let dir = logsDirectory() else { return nil }
        let u = dir.appendingPathComponent(previousFileName)
        return FileManager.default.fileExists(atPath: u.path) ? u : nil
    }

    /// Called once before Bevy boots.  Rotates old log, opens new one, tells Rust.
    static func setup() {
        guard let dir = logsDirectory() else { return }
        let current = dir.appendingPathComponent(sessionFileName)
        let previous = dir.appendingPathComponent(previousFileName)

        // Rotate: existing session log → previous session log (overwrite)
        if FileManager.default.fileExists(atPath: current.path) {
            try? FileManager.default.removeItem(at: previous)
            try? FileManager.default.moveItem(at: current, to: previous)
        }

        // Create fresh session log
        FileManager.default.createFile(atPath: current.path, contents: nil)
        _sessionURL = current

        if let fh = try? FileHandle(forWritingTo: current) {
            _fileHandle = fh
        }

        // Tell Rust: open file + install tracing subscriber
        let prevPath = FileManager.default.fileExists(atPath: previous.path) ? previous.path : ""
        current.path.withCString { cCurrent in
            prevPath.withCString { cPrev in
                jarvis_ios_set_log_file(cCurrent, cPrev)
            }
        }

        JarvisIOSLog.recordHub("JarvisIOSCrashLog: session log at \(current.path)")
    }

    /// Append a Swift log line to the persistent file.  Non-blocking (uses NSLock).
    static func append(_ line: String) {
        guard let fh = _fileHandle else { return }
        let data = (line + "\n").data(using: .utf8) ?? Data()
        writeLock.lock()
        fh.seekToEndOfFile()
        fh.write(data)
        writeLock.unlock()
    }

    /// Upload `prev_session_log.txt` to the desktop hub.  Returns the saved path on success.
    @discardableResult
    static func uploadPreviousSessionLog(hubBaseURL: String) async -> String? {
        guard let logURL = previousSessionLogURL,
              let data = try? Data(contentsOf: logURL),
              !data.isEmpty
        else { return nil }
        guard let base = URL(string: hubBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)),
              base.scheme == "http" || base.scheme == "https"
        else { return nil }

        let endpoint = base.appending(path: "jarvis-ios/v1/log-upload")
        var req = URLRequest(url: endpoint)
        req.httpMethod = "POST"
        req.setValue("text/plain; charset=utf-8", forHTTPHeaderField: "Content-Type")
        req.setValue("prev_session_log.txt", forHTTPHeaderField: "X-Log-Filename")
        let token = HubProfileSync.resolvedHubBearerToken()
        if !token.isEmpty { req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        req.httpBody = data
        req.timeoutInterval = 30

        do {
            let (respData, _) = try await URLSession.shared.data(for: req)
            let msg = String(data: respData, encoding: .utf8) ?? "(no response body)"
            JarvisIOSLog.recordHub("CrashLog upload OK: \(msg)")
            return msg
        } catch {
            JarvisIOSLog.recordHubError("CrashLog upload failed: \(error)")
            return nil
        }
    }

    /// Upload the CURRENT in-memory ring buffer snapshot to the desktop hub.
    @discardableResult
    static func uploadCurrentSnapshot(hubBaseURL: String) async -> Bool {
        let snapshot = jarvis_ios_debug_log_snapshot().toString()
        guard !snapshot.isEmpty else { return false }
        guard let base = URL(string: hubBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)),
              base.scheme == "http" || base.scheme == "https"
        else { return false }

        let endpoint = base.appending(path: "jarvis-ios/v1/log-upload")
        var req = URLRequest(url: endpoint)
        req.httpMethod = "POST"
        req.setValue("text/plain; charset=utf-8", forHTTPHeaderField: "Content-Type")
        req.setValue("live_snapshot.txt", forHTTPHeaderField: "X-Log-Filename")
        let token = HubProfileSync.resolvedHubBearerToken()
        if !token.isEmpty { req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        req.httpBody = snapshot.data(using: .utf8)
        req.timeoutInterval = 30

        do {
            _ = try await URLSession.shared.data(for: req)
            return true
        } catch {
            JarvisIOSLog.recordHubError("Live log push failed: \(error)")
            return false
        }
    }
}

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
        let full = "\(ts) \(line)"
        swiftLines.append(full)
        if swiftLines.count > maxSwiftLines {
            swiftLines.removeFirst(swiftLines.count - maxSwiftLines)
        }
        JarvisIOSCrashLog.append(full)
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
    static let ironclaw = Logger(subsystem: subsystem, category: "Ironclaw")
    /// First-open avatar tab: local motion + IronClaw wiring notes (not hub profile sync).
    static let greeting = Logger(subsystem: subsystem, category: "Greeting")
    static let audio = Logger(subsystem: subsystem, category: "Audio")
    static let camera = Logger(subsystem: subsystem, category: "Camera")

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

    // MARK: IronClaw (hub WS + gateway HTTP/SSE)

    static func recordIronclaw(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Ironclaw] \(message)")
        ironclaw.info("\(message, privacy: .public)")
    }

    static func recordIronclawError(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Ironclaw][error] \(message)")
        ironclaw.error("\(message, privacy: .public)")
    }

    // MARK: First-run avatar greeting (local motion; IronClaw owns persona)

    static func recordGreeting(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Greeting] \(message)")
        greeting.info("\(message, privacy: .public)")
    }

    static func recordGreetingError(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Greeting][error] \(message)")
        greeting.error("\(message, privacy: .public)")
    }

    // MARK: Audio (session + debug mic meter)

    static func recordAudio(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Audio] \(message)")
        audio.info("\(message, privacy: .public)")
    }

    static func recordAudioError(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Audio][error] \(message)")
        audio.error("\(message, privacy: .public)")
    }

    // MARK: Camera (debug preview only)

    static func recordCamera(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Camera] \(message)")
        camera.info("\(message, privacy: .public)")
    }

    static func recordCameraError(_ message: String) {
        JarvisIOSLogBus.appendSwiftLine("[Swift][Camera][error] \(message)")
        camera.error("\(message, privacy: .public)")
    }
}
