import Darwin
import Foundation
import Security

/// Keys for desktop channel hub (same port as WebSocket, typically 6121).
enum HubProfileSync {
    static let userDefaultsBaseURLKey = "jarvis.hub.baseURL"
    static let userDefaultsAuthTokenKey = "jarvis.hub.authToken"

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
        let hub = UserDefaults.standard.string(forKey: userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !hub.isEmpty else { return false }
        JarvisIOSLog.recordHub("syncFromHubToCache start hub=\(hub) hasToken=\(!resolvedAuthToken().isEmpty)")
        do {
            try await downloadHubProfileIntoCacheAndApplyEnv(baseURLString: hub, progress: progress)
            JarvisIOSLog.recordHub("syncFromHubToCache success")
            JarvisIOSLog.logJarvisEnv(JarvisIOSLog.hub, tag: "syncFromHubToCache")
            return true
        } catch {
            JarvisIOSLog.recordHubError("syncFromHubToCache failed: \(String(describing: error))")
            return false
        }
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

    private struct ManifestHeader: Decodable {
        let schema: String
        let revision: UInt64
        let assets: [AssetRef]

        struct AssetRef: Decodable {
            let path: String
            let url: String
        }
    }

    private static func resolvedAuthToken() -> String {
        if let t = JarvisHubKeychain.bearerToken()?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty {
            return t
        }
        return UserDefaults.standard.string(forKey: userDefaultsAuthTokenKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
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

        guard let manifestPath = UserDefaults.standard.string(forKey: userDefaultsCachedManifestPathKey),
              let assetRootPath = UserDefaults.standard.string(forKey: userDefaultsCachedAssetRootKey)
        else {
            JarvisIOSLog.recordHub("applyPersistedHubCacheEnv: miss (missing UserDefaults manifest or asset paths)")
            return false
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
        let token = resolvedAuthToken()

        var manifestRequest = URLRequest(url: manifestURL)
        manifestRequest.httpMethod = "GET"
        if !token.isEmpty {
            manifestRequest.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        let (manifestData, manifestResponse) = try await URLSession.shared.data(for: manifestRequest)
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
            let (fileData, resp) = try await URLSession.shared.data(for: req)
            guard let http = resp as? HTTPURLResponse, (200 ... 299).contains(http.statusCode) else {
                throw URLError(.badServerResponse)
            }
            try fileData.write(to: destURL, options: .atomic)

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
        var query: [String: Any] = [
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
