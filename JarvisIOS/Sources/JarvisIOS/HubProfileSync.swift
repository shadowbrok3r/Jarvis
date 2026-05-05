import Darwin
import Foundation
import Security

/// Longer timeouts + retries for large `.vrm` downloads over LAN / Tailscale (avoids `NSURLErrorDomain` -1005 mid-sync).
private enum JarvisHubDownloadNetworking {
    static let session: URLSession = {
        let c = URLSessionConfiguration.default
        c.timeoutIntervalForRequest = 180
        c.timeoutIntervalForResource = 3600
        c.waitsForConnectivity = true
        return URLSession(configuration: c)
    }()

    static func dataWithRetries(for request: URLRequest, maxAttempts: Int = 4) async throws -> (Data, URLResponse) {
        var last: Error?
        for attempt in 1 ... maxAttempts {
            do {
                return try await session.data(for: request)
            } catch {
                last = error
                if attempt < maxAttempts, isTransientNetworkError(error) {
                    let ns = UInt64(400 + attempt * 350) * 1_000_000
                    try await Task.sleep(nanoseconds: ns)
                    continue
                }
                throw error
            }
        }
        throw last ?? URLError(.unknown)
    }

    /// Downloads directly to `dest` using `URLSession.download(for:)` so the file is never fully buffered
    /// in RAM — essential for large binary assets like `.vrm` / `.vrma` / `.glb` on memory-constrained iOS.
    static func downloadFileStreamingWithRetries(for request: URLRequest, dest: URL, maxAttempts: Int = 4) async throws {
        var last: Error?
        for attempt in 1 ... maxAttempts {
            do {
                let (tmpURL, response) = try await session.download(for: request)
                guard let http = response as? HTTPURLResponse, (200 ... 299).contains(http.statusCode) else {
                    try? FileManager.default.removeItem(at: tmpURL)
                    throw URLError(.badServerResponse)
                }
                let fm = FileManager.default
                if fm.fileExists(atPath: dest.path) {
                    try fm.removeItem(at: dest)
                }
                try fm.moveItem(at: tmpURL, to: dest)
                return
            } catch {
                last = error
                if attempt < maxAttempts, isTransientNetworkError(error) {
                    let ns = UInt64(400 + attempt * 350) * 1_000_000
                    try await Task.sleep(nanoseconds: ns)
                    continue
                }
                throw error
            }
        }
        throw last ?? URLError(.unknown)
    }

    private static func isTransientNetworkError(_ error: Error) -> Bool {
        let e = error as NSError
        guard e.domain == NSURLErrorDomain else { return false }
        switch e.code {
        case NSURLErrorNetworkConnectionLost,
             NSURLErrorTimedOut,
             NSURLErrorCannotConnectToHost,
             NSURLErrorDNSLookupFailed,
             NSURLErrorNotConnectedToInternet,
             NSURLErrorInternationalRoamingOff,
             NSURLErrorCallIsActive,
             NSURLErrorDataNotAllowed:
            return true
        default:
            return false
        }
    }
}

/// Keys for desktop channel hub (same port as WebSocket, typically 6121).
enum HubProfileSync {
    static let userDefaultsBaseURLKey = "jarvis.hub.baseURL"
    /// Optional backup hub (same token). Tried for profile sync + hub WebSocket when primary fails.
    static let userDefaultsSecondaryBaseURLKey = "jarvis.hub.secondaryBaseURL"
    static let userDefaultsAuthTokenKey = "jarvis.hub.authToken"

    /// IronClaw **gateway** (HTTP/SSE chat) — independent of the channel hub URL.
    enum Gateway {
        static let userDefaultsBaseURLKey = "jarvis.gateway.baseURL"
        static let userDefaultsSecondaryBaseURLKey = "jarvis.gateway.secondaryBaseURL"
        static let userDefaultsAuthTokenKey = "jarvis.gateway.authToken"
    }

    /// Kokoro FastAPI (same `/v1/audio/speech` contract as desktop `kokoro_http`).
    enum Kokoro {
        static let userDefaultsBaseURLKey = "jarvis.kokoro.baseURL"
        static let userDefaultsVoiceKey = "jarvis.kokoro.voice"
    }

    /// Local-only overrides (relative to `JARVIS_ASSET_ROOT`); applied via `setenv` before Bevy boot / profile reload.
    enum IosAvatarCustomize {
        static let userDefaultsModelRelPathOverrideKey = "jarvis.ios.overrideModelRelPath"
        static let userDefaultsIdleVrmaRelPathOverrideKey = "jarvis.ios.overrideIdleVrmaRelPath"
    }

    /// Ground + clear-color overrides for the embedded Bevy scene (`JARVIS_IOS_*` env read in Rust).
    enum IosSceneCustomize {
        /// `""` = follow hub manifest; `show` / `hide` force ground visibility.
        static let userDefaultsGroundOverrideKey = "jarvis.ios.scene.groundOverride"
        /// Linear float `r,g,b,a` (e.g. `0.05,0.05,0.08,1`); empty = follow manifest `avatar.background_color`.
        static let userDefaultsBackgroundLinearRgbaKey = "jarvis.ios.scene.backgroundLinearRgba"
    }

    /// Last hub base URL that produced a successful cache (used to avoid reusing cache after URL change).
    private static let userDefaultsCachedHubBaseURLKey = "jarvis.hub.cachedProfileHubBaseURL"
    /// Filesystem paths from the last successful sync (re-applied before Bevy boots without re-downloading).
    private static let userDefaultsCachedManifestPathKey = "jarvis.hub.cachedManifestPath"
    private static let userDefaultsCachedAssetRootKey = "jarvis.hub.cachedAssetRootPath"

    /// Clears `JARVIS_PROFILE_MANIFEST` and points `JARVIS_ASSET_ROOT` at the SwiftPM resource bundle `assets/` tree.
    static func prepareBundledOnlySync() {
        JarvisIOSLog.recordHub("prepareBundledOnlySync: clearing hub manifest, using bundled assets")
        unsetenv("JARVIS_PROFILE_MANIFEST")
        installBundledAssetRootFromSwiftPM()
        JarvisIOSLog.logJarvisEnv(JarvisIOSLog.hub, tag: "prepareBundledOnlySync done")
    }

    /// Drops persisted manifest/asset paths and cached hub string. Not tied to URL field edits (that would
    /// clear on every keystroke); reserved for an explicit “forget hub cache” action if added later.
    static func clearPersistedHubCachePointers() {
        JarvisIOSLog.recordHub("clearPersistedHubCachePointers")
        let keys = [
            userDefaultsCachedHubBaseURLKey,
            userDefaultsCachedManifestPathKey,
            userDefaultsCachedAssetRootKey,
        ]
        for k in keys {
            UserDefaults.standard.removeObject(forKey: k)
        }
    }

    /// Call once at launch (e.g. from `ContentView.onAppear`) so `JARVIS_*` points at the last good
    /// hub cache **before** the user opens Avatar — works offline when the hub URL still matches persisted paths.
    @MainActor
    static func warmUpCachedHubEnvironmentIfPossible() {
        migrateAuthTokenFromUserDefaultsIfNeeded()
        let hub = UserDefaults.standard.string(forKey: userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !hub.isEmpty else { return }
        if applyPersistedHubCacheEnvIfValid(currentHubBaseURL: hub) {
            JarvisIOSLog.recordHub("warmUpCachedHubEnvironment: applied persisted hub cache at launch")
            JarvisIOSLog.logJarvisEnv(JarvisIOSLog.hub, tag: "warmUp")
        }
    }

    /// If hub base URL is empty: bundled assets only. Otherwise: reuse last successful on-disk cache when it
    /// still matches this hub (avoids a second download after About “Sync”, which could fail and wipe env).
    /// If there is no cache yet, downloads manifest + assets. On download failure, re-applies last cache if any.
    static func prepareForBevyBootstrap() async {
        let hub = UserDefaults.standard.string(forKey: userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        JarvisIOSLog.recordHub("prepareForBevyBootstrap hubEmpty=\(hub.isEmpty ? "yes" : "no") hub=\(hub)")
        if hub.isEmpty {
            prepareBundledOnlySync()
            return
        }
        if applyPersistedHubCacheEnvIfValid(currentHubBaseURL: hub) {
            JarvisIOSLog.recordHub("prepareForBevyBootstrap: reused persisted hub cache (no download)")
            JarvisIOSLog.logJarvisEnv(JarvisIOSLog.hub, tag: "prepareForBevyBootstrap cache-hit")
            return
        }
        JarvisIOSLog.recordHub("prepareForBevyBootstrap: downloading profile (no valid cache)")
        do {
            try await downloadHubProfileIntoCacheAndApplyEnv(baseURLString: hub, progress: nil)
            JarvisIOSLog.logJarvisEnv(JarvisIOSLog.hub, tag: "prepareForBevyBootstrap download OK")
        } catch {
            JarvisIOSLog.recordHubError("prepareForBevyBootstrap download failed: \(String(describing: error))")
            if applyPersistedHubCacheEnvIfValid(currentHubBaseURL: hub) {
                JarvisIOSLog.recordHubWarning("prepareForBevyBootstrap: reapplied older persisted cache after failure")
                JarvisIOSLog.logJarvisEnv(JarvisIOSLog.hub, tag: "prepareForBevyBootstrap reapply-cache")
                return
            }
            JarvisIOSLog.recordHubError("prepareForBevyBootstrap: falling back to bundle")
            prepareBundledOnlySync()
        }
    }

    /// Explicit sync (e.g. About tab). Returns whether manifest and all listed assets were cached and env updated.
    /// When `progress` is set, it is updated on the main actor (manifest fetch + each asset; suitable for `ProgressView`).
    @discardableResult
    static func syncFromHubToCache(progress: Progress? = nil) async -> Bool {
        let candidates = hubProfileSyncBaseURLCandidates()
        guard !candidates.isEmpty else { return false }
        JarvisIOSLog.recordHub(
            "syncFromHubToCache start candidates=\(candidates.count) hasToken=\(!resolvedHubBearerToken().isEmpty)"
        )
        var lastError: Error?
        for hub in candidates {
            JarvisIOSLog.recordHub("syncFromHubToCache try hub=\(hub)")
            do {
                try await downloadHubProfileIntoCacheAndApplyEnv(baseURLString: hub, progress: progress)
                JarvisIOSLog.recordHub("syncFromHubToCache success hub=\(hub)")
                JarvisIOSLog.logJarvisEnv(JarvisIOSLog.hub, tag: "syncFromHubToCache")
                return true
            } catch {
                lastError = error
                JarvisIOSLog.recordHubError("syncFromHubToCache failed hub=\(hub): \(String(describing: error))")
            }
        }
        if let lastError {
            JarvisIOSLog.recordHubError("syncFromHubToCache all candidates failed: \(String(describing: lastError))")
        }
        return false
    }

    private static func hubProfileSyncBaseURLCandidates() -> [String] {
        let p = UserDefaults.standard.string(forKey: userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let s = UserDefaults.standard.string(forKey: userDefaultsSecondaryBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        var out: [String] = []
        for raw in [p, s] where !raw.isEmpty {
            let n = normalizedHubBaseURL(raw)
            if !out.contains(n) {
                out.append(n)
            }
        }
        return out
    }

    /// Copy the token field into the Keychain (called from SwiftUI when the user edits or syncs).
    static func persistAuthTokenFromUI(_ token: String) {
        JarvisHubKeychain.setBearerToken(token)
    }

    /// If UserDefaults still holds a token but Keychain is empty (e.g. first run after adding Keychain), migrate.
    static func migrateAuthTokenFromUserDefaultsIfNeeded() {
        JarvisHubKeychain.migrateFromUserDefaultsIfEmpty(userDefaultsKey: userDefaultsAuthTokenKey)
    }

    /// Logs UserDefaults hub/cache keys, on-disk checks, a shallow walk of the newest `rev-*` assets folder,
    /// and current `JARVIS_*` env (for the in-app Logs tab).
    static func logHubCacheDiagnostics() {
        let rawHub = UserDefaults.standard.string(forKey: userDefaultsBaseURLKey) ?? ""
        let hub = rawHub.trimmingCharacters(in: .whitespacesAndNewlines)
        let normHub = normalizedHubBaseURL(hub)
        JarvisIOSLog.recordHub("diag: hub field trimmed=\(hub.isEmpty ? "(empty)" : hub) normalized=\(normHub)")

        let ch = UserDefaults.standard.string(forKey: userDefaultsCachedHubBaseURLKey) ?? ""
        let mp = UserDefaults.standard.string(forKey: userDefaultsCachedManifestPathKey)
        let ar = UserDefaults.standard.string(forKey: userDefaultsCachedAssetRootKey)
        JarvisIOSLog.recordHub("diag: UD cachedHub=\(ch.isEmpty ? "(empty)" : ch) normalized=\(normalizedHubBaseURL(ch))")
        JarvisIOSLog.recordHub("diag: UD manifestPath=\(mp ?? "(nil)")")
        JarvisIOSLog.recordHub("diag: UD assetRootPath=\(ar ?? "(nil)")")

        let match = !ch.isEmpty && normalizedHubBaseURL(ch) == normHub && !normHub.isEmpty
        JarvisIOSLog.recordHub("diag: normalized hub matches cachedHub → \(match) (this is what applyPersisted checks)")

        let fm = FileManager.default
        var isDir: ObjCBool = false
        if let mp {
            let ok = fm.fileExists(atPath: mp, isDirectory: &isDir)
            JarvisIOSLog.recordHub("diag: manifest path exists=\(ok) isDirectory=\(isDir.boolValue)")
        }
        if let ar {
            let ok = fm.fileExists(atPath: ar, isDirectory: &isDir)
            JarvisIOSLog.recordHub("diag: asset root exists=\(ok) isDirectory=\(isDir.boolValue)")
        }

        do {
            let root = try hubProfileCacheRootURL()
            var rootIsDir: ObjCBool = false
            guard fm.fileExists(atPath: root.path, isDirectory: &rootIsDir), rootIsDir.boolValue else {
                JarvisIOSLog.recordHub("diag: no JarvisIOSHubProfile dir at \(root.path)")
                JarvisIOSLog.logJarvisEnv(JarvisIOSLog.hub, tag: "diag env")
                return
            }
            let entries = try fm.contentsOfDirectory(atPath: root.path).sorted()
            JarvisIOSLog.recordHub("diag: cache root \(root.path) dirs=\(entries.joined(separator: ", "))")
            let revs = entries.filter { $0.hasPrefix("rev-") }
            let best = revs.max(by: { revisionSortKey($0) < revisionSortKey($1) })
            if let best {
                let assets = root.appendingPathComponent(best, isDirectory: true).appendingPathComponent("assets", isDirectory: true)
                JarvisIOSLog.recordHub("diag: listing up to 30 paths under \(best)/assets …")
                logPathTreeForDiagnostics(root: assets.path, maxLines: 30)
            } else {
                JarvisIOSLog.recordHub("diag: no rev-* folders under cache root")
            }
        } catch {
            JarvisIOSLog.recordHubError("diag: cache root error \(String(describing: error))")
        }

        JarvisIOSLog.logJarvisEnv(JarvisIOSLog.hub, tag: "diag env")
    }

    // MARK: - Private

    /// If `storedPath` is a **file**, return it. If it is a **directory** (e.g. session `rev-*` folder),
    /// return `…/manifest.json` inside when that path is a regular file.
    private static func resolvedManifestFilePath(storedPath: String) -> String? {
        let fm = FileManager.default
        var isDir: ObjCBool = false
        guard fm.fileExists(atPath: storedPath, isDirectory: &isDir) else { return nil }
        if !isDir.boolValue {
            return storedPath
        }
        let child = (storedPath as NSString).appendingPathComponent("manifest.json")
        var childIsDir: ObjCBool = false
        guard fm.fileExists(atPath: child, isDirectory: &childIsDir), !childIsDir.boolValue else {
            return nil
        }
        return child
    }

    /// When UserDefaults paths are wrong, pick the highest `rev-*` with `manifest.json` + `assets/`.
    private static func discoverLatestCachedRevPaths() -> (manifest: String, assets: String)? {
        let fm = FileManager.default
        guard let root = try? hubProfileCacheRootURL() else { return nil }
        var rootIsDir: ObjCBool = false
        guard fm.fileExists(atPath: root.path, isDirectory: &rootIsDir), rootIsDir.boolValue else { return nil }
        guard let entries = try? fm.contentsOfDirectory(atPath: root.path) else { return nil }
        let revs = entries.filter { $0.hasPrefix("rev-") }
        guard let best = revs.max(by: { revisionSortKey($0) < revisionSortKey($1) }) else { return nil }
        let session = root.appendingPathComponent(best, isDirectory: true)
        let man = session.appendingPathComponent("manifest.json").path
        let assets = session.appendingPathComponent("assets", isDirectory: true).path
        var manDir: ObjCBool = false
        guard fm.fileExists(atPath: man, isDirectory: &manDir), !manDir.boolValue else { return nil }
        var assetsDir: ObjCBool = false
        guard fm.fileExists(atPath: assets, isDirectory: &assetsDir), assetsDir.boolValue else { return nil }
        return (man, assets)
    }

    private struct ManifestHeader: Decodable {
        let schema: String
        let revision: UInt64
        let assets: [AssetRef]

        struct AssetRef: Decodable {
            let path: String
            let url: String
        }
    }

    /// Bearer for hub HTTP (`Authorization`) and WebSocket `module:authenticate` (Keychain first, then UserDefaults).
    static func resolvedHubBearerToken() -> String {
        if let t = JarvisHubKeychain.bearerToken()?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty {
            return t
        }
        return UserDefaults.standard.string(forKey: userDefaultsAuthTokenKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }

    /// `ws://host:6121/ws` from the hub base URL field (same host as profile sync).
    static func hubWebSocketURL() -> URL? {
        hubWebSocketURLCandidates().first
    }

    /// Primary hub WebSocket URL, then optional secondary (same bearer token).
    static func hubWebSocketURLCandidates() -> [URL] {
        let primary = UserDefaults.standard.string(forKey: userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let secondary = UserDefaults.standard.string(forKey: userDefaultsSecondaryBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        var out: [URL] = []
        for raw in [primary, secondary] where !raw.isEmpty {
            guard let u = hubWsURLFromHubBaseString(raw) else { continue }
            if !out.contains(where: { $0.absoluteString == u.absoluteString }) {
                out.append(u)
            }
        }
        return out
    }

    private static func hubWsURLFromHubBaseString(_ raw: String) -> URL? {
        var s = normalizedHubBaseURL(raw)
        guard !s.isEmpty else { return nil }
        if s.hasPrefix("https://") {
            s = "wss://" + String(s.dropFirst(8))
        } else if s.hasPrefix("http://") {
            s = "ws://" + String(s.dropFirst(7))
        } else if !(s.hasPrefix("ws://") || s.hasPrefix("wss://")) {
            return nil
        }
        if s.hasSuffix("/ws") { return URL(string: s) }
        if s.last == "/" {
            return URL(string: s + "ws")
        }
        return URL(string: s + "/ws")
    }

    /// Ordered gateway bases (primary, then optional fallback) for HTTP/SSE.
    static func gatewayBaseURLCandidates() -> [String] {
        let p = UserDefaults.standard.string(forKey: Gateway.userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let f = UserDefaults.standard.string(forKey: Gateway.userDefaultsSecondaryBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        var out: [String] = []
        for s in [p, f] where !s.isEmpty {
            let n = normalizedGatewayBaseURL(s)
            if !out.contains(n) {
                out.append(n)
            }
        }
        return out
    }

    private static func applyPersistedHubCacheEnvIfValid(currentHubBaseURL: String) -> Bool {
        let cachedHub = UserDefaults.standard.string(forKey: userDefaultsCachedHubBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let nc = normalizedHubBaseURL(cachedHub)
        let nu = normalizedHubBaseURL(currentHubBaseURL)
        guard !nc.isEmpty, nc == nu else {
            JarvisIOSLog.recordHub(
                "applyPersistedHubCacheEnv: miss hubMatch=false rawCached=\(cachedHub.isEmpty ? "(empty)" : cachedHub) rawCurrent=\(currentHubBaseURL) normCached=\(nc) normCurrent=\(nu)"
            )
            return false
        }

        guard let manifestPathRaw = UserDefaults.standard.string(forKey: userDefaultsCachedManifestPathKey),
              let assetRootPathRaw = UserDefaults.standard.string(forKey: userDefaultsCachedAssetRootKey)
        else {
            JarvisIOSLog.recordHub("applyPersistedHubCacheEnv: miss (missing UserDefaults manifest or asset paths)")
            return false
        }

        var manifestPathOpt = resolvedManifestFilePath(storedPath: manifestPathRaw)
        var assetRootPath = assetRootPathRaw
        if manifestPathOpt == nil {
            if let pair = discoverLatestCachedRevPaths() {
                JarvisIOSLog.recordHubWarning(
                    "applyPersistedHubCacheEnv: UserDefaults manifest path unusable raw=\(manifestPathRaw); using newest rev-* on disk"
                )
                manifestPathOpt = pair.manifest
                assetRootPath = pair.assets
                persistSuccessfulHubCache(
                    hubBaseURL: currentHubBaseURL,
                    manifestFile: URL(fileURLWithPath: pair.manifest),
                    assetsDir: URL(fileURLWithPath: pair.assets, isDirectory: true)
                )
            }
        }
        guard let manifestPath = manifestPathOpt else {
            JarvisIOSLog.recordHubWarning(
                "applyPersistedHubCacheEnv: could not resolve manifest file raw=\(manifestPathRaw)"
            )
            return false
        }
        if manifestPath != manifestPathRaw {
            UserDefaults.standard.set(manifestPath, forKey: userDefaultsCachedManifestPathKey)
            JarvisIOSLog.recordHub("applyPersistedHubCacheEnv: repaired manifest path in UserDefaults (was directory or session folder)")
        }

        let fm = FileManager.default
        var isDir: ObjCBool = false
        guard fm.fileExists(atPath: manifestPath, isDirectory: &isDir), !isDir.boolValue else {
            JarvisIOSLog.recordHubWarning("applyPersistedHubCacheEnv: manifest missing or is directory path=\(manifestPath)")
            return false
        }
        guard fm.fileExists(atPath: assetRootPath, isDirectory: &isDir), isDir.boolValue else {
            JarvisIOSLog.recordHubWarning("applyPersistedHubCacheEnv: asset root missing or not dir path=\(assetRootPath)")
            return false
        }

        setenv("JARVIS_ASSET_ROOT", assetRootPath, 1)
        setenv("JARVIS_PROFILE_MANIFEST", manifestPath, 1)
        JarvisIOSLog.recordHub("applyPersistedHubCacheEnv: setenv OK manifest=\(manifestPath) assets=\(assetRootPath)")
        return true
    }

    private static func persistSuccessfulHubCache(
        hubBaseURL: String,
        manifestFile: URL,
        assetsDir: URL
    ) {
        let hub = normalizedHubBaseURL(hubBaseURL)
        UserDefaults.standard.set(hub, forKey: userDefaultsCachedHubBaseURLKey)
        UserDefaults.standard.set(manifestFile.path, forKey: userDefaultsCachedManifestPathKey)
        UserDefaults.standard.set(assetsDir.path, forKey: userDefaultsCachedAssetRootKey)
    }

    private static func downloadHubProfileIntoCacheAndApplyEnv(baseURLString: String, progress: Progress?) async throws {
        guard let baseURL = URL(string: baseURLString),
              let scheme = baseURL.scheme?.lowercased(),
              scheme == "http" || scheme == "https"
        else {
            throw URLError(.badURL)
        }

        await updateHubDownloadProgress(progress, completed: 0, total: 1, localizedDescription: "Fetching manifest…")

        let manifestURL = baseURL.appending(path: "jarvis-ios/v1/manifest")
        let token = resolvedHubBearerToken()

        var manifestRequest = URLRequest(url: manifestURL)
        manifestRequest.httpMethod = "GET"
        if !token.isEmpty {
            manifestRequest.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        let (manifestData, manifestResponse) = try await JarvisHubDownloadNetworking.dataWithRetries(for: manifestRequest)
        guard let httpManifest = manifestResponse as? HTTPURLResponse,
              (200 ... 299).contains(httpManifest.statusCode)
        else {
            throw URLError(.badServerResponse)
        }

        let header = try JSONDecoder().decode(ManifestHeader.self, from: manifestData)
        guard header.schema == "jarvis-ios.profile.v1" else {
            throw URLError(.cannotParseResponse)
        }

        let n = header.assets.count
        let totalSteps = Int64(1 + max(n, 0))
        await updateHubDownloadProgress(progress, completed: 1, total: totalSteps, localizedDescription: n == 0 ? "Saving…" : "Downloading \(n) asset(s)…")

        let fm = FileManager.default
        let support = try fm.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
        let cacheRoot = support.appendingPathComponent("JarvisIOSHubProfile", isDirectory: true)
        let sessionDir = cacheRoot.appendingPathComponent("rev-\(header.revision)", isDirectory: true)
        let assetsDir = sessionDir.appendingPathComponent("assets", isDirectory: true)
        try fm.createDirectory(at: assetsDir, withIntermediateDirectories: true)

        for (index, asset) in header.assets.enumerated() {
            guard isSafeAssetsRelPath(asset.path) else {
                throw NSError(domain: "HubProfileSync", code: 2, userInfo: [NSLocalizedDescriptionKey: "Unsafe asset path"])
            }
            let destURL = fileURL(directory: assetsDir, relativePOSIXPath: asset.path)
            try fm.createDirectory(at: destURL.deletingLastPathComponent(), withIntermediateDirectories: true)

            guard let downloadURL = resolveAssetURL(assetURLString: asset.url, baseURL: baseURL) else {
                throw URLError(.badURL)
            }
            let assetLabel = (asset.path as NSString).lastPathComponent

            var req = URLRequest(url: downloadURL)
            req.httpMethod = "GET"
            if !token.isEmpty {
                req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            }

            // Large binary assets (VRM/VRMA/GLB) are streamed directly to disk so the full file
            // is never held in RAM as a Data object.  Everything else (JSON, TOML, …) uses the
            // in-memory path which is simpler and fine for small payloads.
            let pathExt = (asset.path as NSString).pathExtension.lowercased()
            let isLargeBinary = pathExt == "vrm" || pathExt == "vrma" || pathExt == "glb"
            if isLargeBinary {
                try await JarvisHubDownloadNetworking.downloadFileStreamingWithRetries(for: req, dest: destURL)
            } else {
                let (fileData, resp) = try await JarvisHubDownloadNetworking.dataWithRetries(for: req)
                guard let http = resp as? HTTPURLResponse, (200 ... 299).contains(http.statusCode) else {
                    throw URLError(.badServerResponse)
                }
                try fileData.write(to: destURL, options: .atomic)
            }

            let done = index + 1
            let cap = done == n ? "Finishing…" : "Downloaded \(assetLabel) (\(done)/\(n))"
            await updateHubDownloadProgress(
                progress,
                completed: Int64(2 + index),
                total: totalSteps,
                localizedDescription: cap
            )
        }

        let manifestFile = sessionDir.appendingPathComponent("manifest.json")
        try manifestData.write(to: manifestFile, options: .atomic)

        setenv("JARVIS_ASSET_ROOT", assetsDir.path, 1)
        setenv("JARVIS_PROFILE_MANIFEST", manifestFile.path, 1)
        JarvisIOSLog.recordHub("downloadHubProfile: setenv assets=\(assetsDir.path) manifest=\(manifestFile.path)")

        persistSuccessfulHubCache(
            hubBaseURL: normalizedHubBaseURL(baseURLString.trimmingCharacters(in: .whitespacesAndNewlines)),
            manifestFile: manifestFile,
            assetsDir: assetsDir
        )

        await updateHubDownloadProgress(progress, completed: totalSteps, total: totalSteps, localizedDescription: "Done")
    }

    /// `Progress` is updated on the main queue so SwiftUI can observe `fractionCompleted`.
    private static func updateHubDownloadProgress(
        _ progress: Progress?,
        completed: Int64,
        total: Int64,
        localizedDescription: String
    ) async {
        guard let progress else { return }
        let t = max(1, total)
        let c = min(max(0, completed), t)
        await MainActor.run {
            progress.totalUnitCount = t
            progress.completedUnitCount = c
            progress.localizedDescription = localizedDescription
        }
    }

    private static func resolveAssetURL(assetURLString: String, baseURL: URL) -> URL? {
        let s = assetURLString.trimmingCharacters(in: .whitespacesAndNewlines)
        if s.isEmpty { return nil }
        if let abs = URL(string: s), abs.scheme != nil {
            return abs
        }
        return URL(string: s, relativeTo: baseURL)?.absoluteURL
    }

    /// Match `jarvis_ios_hub::is_safe_assets_rel` (no `..`, no absolute).
    private static func isSafeAssetsRelPath(_ rel: String) -> Bool {
        if rel.isEmpty || rel.hasPrefix("/") { return false }
        for part in rel.split(separator: "/") {
            if part == ".." || part == "/" { return false }
        }
        return true
    }

    private static func fileURL(directory: URL, relativePOSIXPath: String) -> URL {
        var u = directory
        for part in relativePOSIXPath.split(separator: "/") where !part.isEmpty {
            u = u.appendingPathComponent(String(part))
        }
        return u
    }

    /// Same logic as former `installJarvisBundledAssetRoot` in `JarvisBevyView`.
    /// Trims whitespace and strips trailing `/` so `http://h:6121` matches `http://h:6121/` in cache keys.
    private static func normalizedHubBaseURL(_ raw: String) -> String {
        var s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while s.count > 1, s.last == "/" {
            s.removeLast()
        }
        return s
    }

    private static func hubProfileCacheRootURL() throws -> URL {
        let fm = FileManager.default
        let support = try fm.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false
        )
        return support.appendingPathComponent("JarvisIOSHubProfile", isDirectory: true)
    }

    private static func revisionSortKey(_ revFolderName: String) -> UInt64 {
        let prefix = "rev-"
        guard revFolderName.hasPrefix(prefix) else { return 0 }
        let rest = String(revFolderName.dropFirst(prefix.count))
        return UInt64(rest) ?? 0
    }

    private static func logPathTreeForDiagnostics(root: String, maxLines: Int) {
        let fm = FileManager.default
        var isDir: ObjCBool = false
        guard fm.fileExists(atPath: root, isDirectory: &isDir), isDir.boolValue else {
            JarvisIOSLog.recordHub("diag: assets path missing or not dir: \(root)")
            return
        }
        guard let en = fm.enumerator(atPath: root) else { return }
        var n = 0
        while let rel = en.nextObject() as? String {
            JarvisIOSLog.recordHub("diag: · \(rel)")
            n += 1
            if n >= maxLines {
                JarvisIOSLog.recordHub("diag: … truncated (\(maxLines) paths)")
                break
            }
        }
        if n == 0 {
            JarvisIOSLog.recordHub("diag: assets folder empty")
        }
    }

    private static func installBundledAssetRootFromSwiftPM() {
        for bundle in [Bundle.module, Bundle.main] {
            guard let base = bundle.resourceURL else {
                JarvisIOSLog.recordHubDebug("installBundledAssetRoot: bundle has no resourceURL (skipping)")
                continue
            }
            let assetsURL = base.appendingPathComponent("assets", isDirectory: true)
            var isDir: ObjCBool = false
            guard FileManager.default.fileExists(atPath: assetsURL.path, isDirectory: &isDir), isDir.boolValue else {
                JarvisIOSLog.recordHubDebug("installBundledAssetRoot: no assets dir at \(assetsURL.path)")
                continue
            }
            setenv("JARVIS_ASSET_ROOT", assetsURL.path, 1)
            JarvisIOSLog.recordHub("installBundledAssetRoot: JARVIS_ASSET_ROOT=\(assetsURL.path)")
            return
        }
        JarvisIOSLog.recordHubError("installBundledAssetRoot: FAILED — no assets folder in Bundle.module or Bundle.main")
    }
}

// MARK: - Keychain (bearer token)

private enum JarvisHubKeychain {
    private static let service = "JarvisIOS.hub"
    private static let account = "ironclaw.bearer"

    static func setBearerToken(_ token: String) {
        let data = Data(token.utf8)
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(base as CFDictionary)
        guard !token.isEmpty else { return }
        var query = base
        query[kSecValueData as String] = data
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(query as CFDictionary, nil)
    }

    static func bearerToken() -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var out: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &out)
        guard status == errSecSuccess, let data = out as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func migrateFromUserDefaultsIfEmpty(userDefaultsKey: String) {
        guard bearerToken() == nil else { return }
        let ud = UserDefaults.standard.string(forKey: userDefaultsKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !ud.isEmpty else { return }
        setBearerToken(ud)
    }
}

// MARK: - Keychain (IronClaw gateway bearer)

private enum JarvisGatewayKeychain {
    private static let service = "JarvisIOS.gateway"
    private static let account = "bearer"

    static func setBearerToken(_ token: String) {
        let data = Data(token.utf8)
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(base as CFDictionary)
        guard !token.isEmpty else { return }
        var query = base
        query[kSecValueData as String] = data
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(query as CFDictionary, nil)
    }

    static func bearerToken() -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var out: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &out)
        guard status == errSecSuccess, let data = out as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func migrateFromUserDefaultsIfEmpty(userDefaultsKey: String) {
        guard bearerToken() == nil else { return }
        let ud = UserDefaults.standard.string(forKey: userDefaultsKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !ud.isEmpty else { return }
        setBearerToken(ud)
    }
}

extension HubProfileSync {
    static func migrateGatewayAuthTokenFromUserDefaultsIfNeeded() {
        JarvisGatewayKeychain.migrateFromUserDefaultsIfEmpty(userDefaultsKey: Gateway.userDefaultsAuthTokenKey)
    }

    static func persistGatewayAuthTokenFromUI(_ token: String) {
        UserDefaults.standard.set(token, forKey: Gateway.userDefaultsAuthTokenKey)
        JarvisGatewayKeychain.setBearerToken(token)
    }

    static func resolvedGatewayBearerToken() -> String {
        if let t = JarvisGatewayKeychain.bearerToken()?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty {
            return t
        }
        return UserDefaults.standard.string(forKey: Gateway.userDefaultsAuthTokenKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }

    static func normalizedGatewayBaseURL(_ raw: String) -> String {
        var s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while s.count > 1, s.last == "/" {
            s.removeLast()
        }
        return s
    }

    // MARK: - iOS avatar model (VRM / VRMA overrides + discovery)

    /// Pushes `JARVIS_IOS_MODEL_PATH` / `JARVIS_IOS_IDLE_VRMA_PATH` for the Rust `jarvis_ios` staticlib.
    /// Call before `jarvis_renderer_new` and before `jarvis_renderer_reload_profile`.
    static func applyIosAvatarOverrideEnvFromUserDefaults() {
        let m = UserDefaults.standard.string(forKey: IosAvatarCustomize.userDefaultsModelRelPathOverrideKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if m.isEmpty || m.contains("..") || m.hasPrefix("/") {
            unsetenv("JARVIS_IOS_MODEL_PATH")
        } else {
            setenv("JARVIS_IOS_MODEL_PATH", m, 1)
        }

        let idle = UserDefaults.standard.string(forKey: IosAvatarCustomize.userDefaultsIdleVrmaRelPathOverrideKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if idle.isEmpty {
            unsetenv("JARVIS_IOS_IDLE_VRMA_PATH")
        } else if idle.contains("..") || idle.hasPrefix("/") {
            unsetenv("JARVIS_IOS_IDLE_VRMA_PATH")
        } else {
            setenv("JARVIS_IOS_IDLE_VRMA_PATH", idle, 1)
        }

        let ground = UserDefaults.standard.string(forKey: IosSceneCustomize.userDefaultsGroundOverrideKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased() ?? ""
        switch ground {
        case "show":
            setenv("JARVIS_IOS_SHOW_GROUND", "1", 1)
        case "hide":
            setenv("JARVIS_IOS_SHOW_GROUND", "0", 1)
        default:
            unsetenv("JARVIS_IOS_SHOW_GROUND")
        }

        let bg = UserDefaults.standard.string(forKey: IosSceneCustomize.userDefaultsBackgroundLinearRgbaKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if bg.isEmpty {
            unsetenv("JARVIS_IOS_BACKGROUND_LINEAR")
        } else {
            setenv("JARVIS_IOS_BACKGROUND_LINEAR", bg, 1)
        }
    }

    /// `avatar.model_path` from the last synced hub `manifest.json` (for UI labels).
    static func readHubManifestModelPath() -> String? {
        guard let path = UserDefaults.standard.string(forKey: userDefaultsCachedManifestPathKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines), !path.isEmpty
        else { return nil }
        let manifestURL: URL
        var isDir: ObjCBool = false
        if FileManager.default.fileExists(atPath: path, isDirectory: &isDir), isDir.boolValue {
            manifestURL = URL(fileURLWithPath: path, isDirectory: true).appendingPathComponent("manifest.json")
        } else {
            manifestURL = URL(fileURLWithPath: path)
        }
        guard let data = try? Data(contentsOf: manifestURL),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let av = obj["avatar"] as? [String: Any],
              let mp = av["model_path"] as? String
        else { return nil }
        return mp.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Active `JARVIS_ASSET_ROOT` directory: process env first, then persisted hub cache asset folder.
    static func resolvedAssetRootDirectoryURL() -> URL? {
        if let ar = JarvisIOSLog.getenvString("JARVIS_ASSET_ROOT")?.trimmingCharacters(in: .whitespacesAndNewlines),
           !ar.isEmpty
        {
            let u = URL(fileURLWithPath: ar, isDirectory: true)
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: u.path, isDirectory: &isDir), isDir.boolValue {
                return u
            }
        }
        if let s = UserDefaults.standard.string(forKey: userDefaultsCachedAssetRootKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines), !s.isEmpty
        {
            let u = URL(fileURLWithPath: s, isDirectory: true)
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: u.path, isDirectory: &isDir), isDir.boolValue {
                return u
            }
        }
        return nil
    }

    /// All `.vrm` files under the resolved asset root, as repo-relative paths (e.g. `models/foo.vrm`).
    static func listDiscoveredVrmRelativePaths(maxFiles: Int = 300) -> [String] {
        guard let root = resolvedAssetRootDirectoryURL() else { return [] }
        let fm = FileManager.default
        guard let en = fm.enumerator(
            at: root,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        ) else { return [] }

        var out: [String] = []
        while out.count < maxFiles, let u = en.nextObject() as? URL {
            guard u.pathExtension.lowercased() == "vrm" else { continue }
            guard let rel = vrmPathRelativeToAssetRoot(file: u, assetRoot: root) else { continue }
            out.append(rel)
        }
        return out.sorted()
    }

    /// JSON animation files under the resolved asset root (e.g. pose-library exports), as paths relative to that root.
    /// Used by the ACT / emotion mapping editor to match desktop `EmotionBinding.animation` filenames.
    static func listDiscoveredAnimationJsonRelativePaths(maxFiles: Int = 400) -> [String] {
        guard let root = resolvedAssetRootDirectoryURL() else { return [] }
        let fm = FileManager.default
        guard let en = fm.enumerator(
            at: root,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        ) else { return [] }

        var out: [String] = []
        while out.count < maxFiles, let u = en.nextObject() as? URL {
            guard u.pathExtension.lowercased() == "json" else { continue }
            let name = u.lastPathComponent.lowercased()
            if name == "manifest.json" { continue }
            guard let rel = vrmPathRelativeToAssetRoot(file: u, assetRoot: root) else { continue }
            out.append(rel)
        }
        return out.sorted()
    }

    private static func jarvisApplicationSupportDirectory() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return base.appendingPathComponent("JarvisIOS", isDirectory: true)
    }

    /// Primary: `config/emotions.json` under the hub / bundled asset root. Fallback: Application Support when no root.
    static func resolvedEmotionsJsonFileURL() -> URL {
        if let root = resolvedAssetRootDirectoryURL() {
            return root.appendingPathComponent("config/emotions.json", isDirectory: false)
        }
        let dir = jarvisApplicationSupportDirectory()
        return dir.appendingPathComponent("emotions.json", isDirectory: false)
    }

    /// Ensures parent directories exist before writing `resolvedEmotionsJsonFileURL()`.
    static func ensureParentDirectoryExists(for fileURL: URL) {
        let parent = fileURL.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
    }

    /// `dropFirst(root.path.count)` is unsafe on iOS: enumerator paths are often `/private/var/…` while
    /// `URL.path` for the same root may be `/var/…`, producing garbage like `1/assets/models/…` and a broken
    /// `JARVIS_IOS_MODEL_PATH` (Bevy then looks under `…/assets/1/assets/…`).
    fileprivate static func vrmPathRelativeToAssetRoot(file: URL, assetRoot: URL) -> String? {
        let rootP = posixPathForComparison(assetRoot)
        let fileP = posixPathForComparison(file)
        guard fileP.hasPrefix(rootP), fileP.count > rootP.count else { return nil }
        var rel = String(fileP.dropFirst(rootP.count))
        if rel.hasPrefix("/") { rel.removeFirst() }
        guard !rel.isEmpty, !rel.contains("..") else { return nil }
        return rel
    }

    /// Collapses `/private/var` → `/var` so two URLs that point at the same directory compare equal.
    fileprivate static func posixPathForComparison(_ url: URL) -> String {
        var p = url.resolvingSymlinksInPath().standardizedFileURL.path
        let privateVar = "/private/var/"
        let plainVar = "/var/"
        if p.hasPrefix(privateVar) {
            p = plainVar + String(p.dropFirst(privateVar.count))
        }
        while p.count > 1, p.last == "/" {
            p.removeLast()
        }
        return p
    }

    /// Keys already present in `config/emotions.json` (lowercased ACT labels).
    static func mappedEmotionKeysLowercased() -> Set<String> {
        let url = resolvedEmotionsJsonFileURL()
        guard let data = try? Data(contentsOf: url),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let mappings = root["mappings"] as? [String: Any]
        else {
            return []
        }
        return Set(mappings.keys.map { $0.lowercased() })
    }

    /// Adds minimal placeholder rows for unknown ACT emotion labels (desktop-compatible `emotions.json`).
    static func ensurePlaceholderEmotions(for labels: [String]) {
        let keys = mappedEmotionKeysLowercased()
        var toAdd: [String] = []
        for raw in labels {
            let k = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            guard !k.isEmpty, !keys.contains(k), !toAdd.contains(k) else { continue }
            toAdd.append(k)
        }
        guard !toAdd.isEmpty else { return }

        let url = resolvedEmotionsJsonFileURL()
        ensureParentDirectoryExists(for: url)
        var mappings: [String: Any] = [:]
        if let data = try? Data(contentsOf: url),
           let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let existing = root["mappings"] as? [String: Any]
        {
            mappings = existing
        }
        for k in toAdd {
            mappings[k] = [
                "notes": "auto placeholder (Jarvis iOS chat — map expression / animation in About)",
                "hold_seconds": 2.5,
            ] as [String: Any]
        }
        let out: [String: Any] = ["mappings": mappings]
        guard let data = try? JSONSerialization.data(withJSONObject: out, options: [.prettyPrinted, .sortedKeys])
        else { return }
        try? data.write(to: url, options: [.atomic])
        JarvisIOSLog.recordUI("emotions.json placeholders added: \(toAdd.joined(separator: ", "))")
    }
}
