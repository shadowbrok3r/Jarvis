import Foundation

/// First time the embedded Bevy avatar surface comes up: play a short local motion via existing FFI.
/// Persona, memory, and system prompts stay on **IronClaw** — this layer does not embed assistant prompts.
enum AvatarFirstRunGreeting {
    private static let userDefaultsPrefix = "jarvis.avatar.did_first_greeting."

    /// VRMA / JSON paths relative to `JARVIS_ASSET_ROOT` (see `JarvisIOS/README.md` “First-run greeting”).
    private static let vrmaCandidates: [(rel: String, loopForever: Bool)] = [
        ("models/wave.vrma", false),
        ("models/idle_loop.vrma", false),
    ]

    private static let animJsonCandidates: [String] = [
        "animations/talk_nod.json",
    ]

    /// Stable token for `UserDefaults` (manifest path, device override, or bundled default).
    static func identityTokenForUserDefaults() -> String {
        let o = UserDefaults.standard.string(forKey: HubProfileSync.IosAvatarCustomize.userDefaultsModelRelPathOverrideKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !o.isEmpty { return sanitizeKeySegment(o) }
        if let m = HubProfileSync.readHubManifestModelPath(), !m.isEmpty { return sanitizeKeySegment(m) }
        return "bundled-default"
    }

    private static func sanitizeKeySegment(_ s: String) -> String {
        let bad = CharacterSet(charactersIn: "/\\:?%*\"<>|")
        return s.components(separatedBy: bad).joined(separator: "_")
    }

    private static var didCompleteKey: String {
        userDefaultsPrefix + identityTokenForUserDefaults()
    }

    private static func isSafeAssetRel(_ rel: String) -> Bool {
        let t = rel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty, !t.hasPrefix("/"), !t.contains("..") else { return false }
        return true
    }

    private static func fileExistsUnderAssetRoot(relative: String) -> Bool {
        guard isSafeAssetRel(relative) else { return false }
        guard let root = HubProfileSync.resolvedAssetRootDirectoryURL() else { return false }
        let url = root.appendingPathComponent(relative, isDirectory: false)
        var isDir: ObjCBool = false
        return FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir) && !isDir.boolValue
    }

    /// Called from `JarvisBevyView` after `jarvis_renderer_new` + display link attach so the avatar root exists in Rust.
    @MainActor
    static func scheduleIfNeededAfterBootstrap() {
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 650_000_000)
            performIfNeeded()
        }
    }

    @MainActor
    private static func performIfNeeded() {
        guard JarvisBevySession.hasRegisteredRenderer() else {
            JarvisIOSLog.recordGreeting("skip: no renderer (tab or teardown)")
            return
        }
        let key = didCompleteKey
        if UserDefaults.standard.bool(forKey: key) {
            JarvisIOSLog.recordGreeting("skip: already completed for identity=\(identityTokenForUserDefaults())")
            return
        }

        if let (rel, loopForever) = vrmaCandidates.first(where: { fileExistsUnderAssetRoot(relative: $0.rel) }) {
            JarvisBevySession.queueVrma(path: rel, loopForever: loopForever)
            JarvisIOSLog.recordGreeting("local first-open VRMA rel=\(rel) loopForever=\(loopForever)")
            UserDefaults.standard.set(true, forKey: key)
            logIronclawNote()
            return
        }

        if let rel = animJsonCandidates.first(where: { fileExistsUnderAssetRoot(relative: $0) }) {
            JarvisBevySession.queueAnimJson(path: rel)
            JarvisIOSLog.recordGreeting("local first-open anim JSON rel=\(rel)")
            UserDefaults.standard.set(true, forKey: key)
            logIronclawNote()
            return
        }

        JarvisIOSLog.recordGreeting(
            "local motion skipped — no candidate files under asset root (add models/wave.vrma, models/idle_loop.vrma, or animations/talk_nod.json; sync hub). didComplete key not set."
        )
        logIronclawNote()
    }

    /// IronClaw gateway chat today requires non-empty user content for `POST /api/chat/send`; there is no assistant-only greet SSE in this client.
    private static func logIronclawNote() {
        JarvisIOSLog.recordGreeting(
            "IronClaw: no in-app auto-send for opening line — persona lives on gateway; see README “First-run greeting”. TODO: optional greet endpoint or explicit user nudge policy."
        )
    }
}
