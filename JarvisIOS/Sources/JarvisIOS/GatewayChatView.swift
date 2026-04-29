import SwiftUI
import PhotosUI
import UIKit

// MARK: - DTOs (mirror `src/ironclaw/types.rs`)

private struct ThreadInfoDTO: Codable {
    let id: String
    let state: String
    let turnCount: Int
    let createdAt: String
    let updatedAt: String
    let title: String?
}

private struct ThreadListDTO: Codable {
    let assistantThread: ThreadInfoDTO?
    let threads: [ThreadInfoDTO]?
    let activeThread: String?

    var threadsNonEmpty: [ThreadInfoDTO] { threads ?? [] }
}

/// Mirrors `ironclaw::types::HistoryResponse` / `GET /api/chat/history`.
private struct HistoryResponseDTO: Codable {
    let threadId: String
    let turns: [TurnInfoDTO]?
    let hasMore: Bool?
}

/// Mirrors `ironclaw::types::TurnInfo` (snake_case on wire).
private struct TurnInfoDTO: Codable {
    let userInput: String
    let response: String?
    /// Gateway may send model reasoning as `thinking` or `reasoning` (see Rust `#[serde(alias)]`).
    let thinking: String?
    let reasoning: String?
}

private struct ImageDataDTO: Encodable {
    let mediaType: String
    let data: String
}

private struct SendMessageBody: Encodable {
    let content: String
    let threadId: String?
    let timezone: String?
    let images: [ImageDataDTO]
}

private struct SendMessageResponseDTO: Codable {
    let messageId: String
    let status: String
}

private enum GatewayHTTPError: LocalizedError {
    case badURL
    case badStatus(Int, String)
    case authRejected

    var errorDescription: String? {
        switch self {
        case .badURL: return "Invalid gateway URL"
        case .badStatus(let code, let body): return "HTTP \(code): \(body.prefix(200))"
        case .authRejected: return "Gateway rejected the bearer token (401/403)"
        }
    }
}

// MARK: - HTTP client (parity with `src/ironclaw/client.rs`)

private enum IronclawGatewayHTTP {
    /// Avoid intermediaries buffering SSE bodies; identity encoding for small JSON bodies.
    private static func applyGatewayProxies(_ r: inout URLRequest) {
        r.setValue("identity", forHTTPHeaderField: "Accept-Encoding")
        r.setValue("no-cache", forHTTPHeaderField: "Cache-Control")
    }

    private static func jsonDecoder() -> JSONDecoder {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }

    private static func jsonEncoder() -> JSONEncoder {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }

    private static func authorizedGET(url: URL, bearer: String) -> URLRequest {
        var r = URLRequest(url: url)
        r.httpMethod = "GET"
        if !bearer.isEmpty {
            r.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        }
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        applyGatewayProxies(&r)
        r.timeoutInterval = 120
        return r
    }

    private static func authorizedPOSTJSON(url: URL, bearer: String, body: Data) -> URLRequest {
        var r = URLRequest(url: url)
        r.httpMethod = "POST"
        if !bearer.isEmpty {
            r.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        }
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        applyGatewayProxies(&r)
        r.httpBody = body
        r.timeoutInterval = 120
        return r
    }

    private static func authorizedSSE(url: URL, bearer: String) -> URLRequest {
        var r = URLRequest(url: url)
        r.httpMethod = "GET"
        if !bearer.isEmpty {
            r.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        }
        r.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        applyGatewayProxies(&r)
        r.timeoutInterval = 60 * 60 * 24
        return r
    }

    static func listThreads(baseURL: String, bearer: String) async throws -> ThreadListDTO {
        let base = HubProfileSync.normalizedGatewayBaseURL(baseURL)
        guard let url = URL(string: base + "/api/chat/threads") else { throw GatewayHTTPError.badURL }
        let (data, resp) = try await URLSession.shared.data(for: authorizedGET(url: url, bearer: bearer))
        try throwIfNeeded(resp, data)
        return try jsonDecoder().decode(ThreadListDTO.self, from: data)
    }

    /// Same contract as Rust `GatewayClient::history` (`thread_id`, optional `limit`).
    static func fetchHistory(
        baseURL: String,
        bearer: String,
        threadId: String,
        limit: UInt32
    ) async throws -> HistoryResponseDTO {
        let base = HubProfileSync.normalizedGatewayBaseURL(baseURL)
        var comp = URLComponents(string: base + "/api/chat/history")
        comp?.queryItems = [
            URLQueryItem(name: "thread_id", value: threadId),
            URLQueryItem(name: "limit", value: String(limit)),
        ]
        guard let url = comp?.url else { throw GatewayHTTPError.badURL }
        let (data, resp) = try await URLSession.shared.data(for: authorizedGET(url: url, bearer: bearer))
        try throwIfNeeded(resp, data)
        return try jsonDecoder().decode(HistoryResponseDTO.self, from: data)
    }

    static func createThread(baseURL: String, bearer: String) async throws -> ThreadInfoDTO {
        let base = HubProfileSync.normalizedGatewayBaseURL(baseURL)
        guard let url = URL(string: base + "/api/chat/thread/new") else { throw GatewayHTTPError.badURL }
        let (data, resp) = try await URLSession.shared.data(for: authorizedPOSTJSON(url: url, bearer: bearer, body: Data()))
        try throwIfNeeded(resp, data)
        return try jsonDecoder().decode(ThreadInfoDTO.self, from: data)
    }

    static func sendMessage(
        baseURL: String,
        bearer: String,
        content: String,
        threadId: String?,
        images: [ImageDataDTO]
    ) async throws -> SendMessageResponseDTO {
        let base = HubProfileSync.normalizedGatewayBaseURL(baseURL)
        guard let url = URL(string: base + "/api/chat/send") else { throw GatewayHTTPError.badURL }
        let body = SendMessageBody(
            content: content,
            threadId: threadId,
            timezone: TimeZone.current.identifier,
            images: images
        )
        let enc = try jsonEncoder().encode(body)
        let (data, resp) = try await URLSession.shared.data(for: authorizedPOSTJSON(url: url, bearer: bearer, body: enc))
        try throwIfNeeded(resp, data)
        return try jsonDecoder().decode(SendMessageResponseDTO.self, from: data)
    }

    static func openEventsRequest(baseURL: String, bearer: String) throws -> URLRequest {
        let base = HubProfileSync.normalizedGatewayBaseURL(baseURL)
        guard let url = URL(string: base + "/api/chat/events") else { throw GatewayHTTPError.badURL }
        return authorizedSSE(url: url, bearer: bearer)
    }

    private static func throwIfNeeded(_ resp: URLResponse, _ data: Data) throws {
        guard let http = resp as? HTTPURLResponse else { return }
        guard (200 ... 299).contains(http.statusCode) else {
            let text = String(data: data, encoding: .utf8) ?? ""
            if http.statusCode == 401 || http.statusCode == 403 {
                throw GatewayHTTPError.authRejected
            }
            throw GatewayHTTPError.badStatus(http.statusCode, text)
        }
    }
}

// MARK: - SSE payloads (`AppEvent`-shaped)

private enum ParsedGatewayEvent {
    case response(content: String, threadId: String?)
    case streamChunk(content: String, threadId: String?)
    case thinking(message: String)
    case status(message: String)
    case toolStarted(name: String, detail: String?)
    case toolCompleted(name: String, success: Bool, error: String?)
    case toolResult(name: String, preview: String)
    case error(message: String)
    case other(type: String)

    static func parse(jsonLine: String) -> ParsedGatewayEvent? {
        guard let d = jsonLine.data(using: .utf8),
              let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
              let typ = o["type"] as? String
        else { return nil }

        switch typ {
        case "response":
            return .response(content: o["content"] as? String ?? "", threadId: o["thread_id"] as? String)
        case "stream_chunk":
            return .streamChunk(content: o["content"] as? String ?? "", threadId: o["thread_id"] as? String)
        case "thinking":
            return .thinking(message: o["message"] as? String ?? "")
        case "status":
            return .status(message: o["message"] as? String ?? "")
        case "tool_started":
            return .toolStarted(name: o["name"] as? String ?? "", detail: o["detail"] as? String)
        case "tool_completed":
            return .toolCompleted(
                name: o["name"] as? String ?? "",
                success: o["success"] as? Bool ?? false,
                error: o["error"] as? String
            )
        case "tool_result":
            return .toolResult(name: o["name"] as? String ?? "", preview: o["preview"] as? String ?? "")
        case "error":
            return .error(message: o["message"] as? String ?? "error")
        default:
            return .other(type: typ)
        }
    }
}

// MARK: - View model

@Observable @MainActor
final class GatewayChatViewModel {
    /// Nested so `@Observable` storage does not clash with `private(fileprivate)` visibility rules (Swift 6).
    struct ChatLine: Identifiable {
        struct InlineImage: Identifiable, Equatable {
            let id: UUID
            let mediaType: String
            let data: Data

            init(id: UUID = UUID(), mediaType: String, data: Data) {
                self.id = id
                self.mediaType = mediaType
                self.data = data
            }
        }

        enum Role {
            case user
            case assistant
            case system
        }

        let id: UUID
        let role: Role
        var text: String
        var images: [InlineImage]

        init(role: Role, text: String, images: [InlineImage] = []) {
            self.id = UUID()
            self.role = role
            self.text = text
            self.images = images
        }
    }

    struct PendingImage: Identifiable, Equatable {
        let id: UUID
        let mediaType: String
        let data: Data
    }

    var draft: String = ""
    /// Staged gateway attachments (cleared after a successful send).
    var pendingImages: [PendingImage] = []
    var lines: [ChatLine] = []
    var liveAssistant: String = ""
    let liveScrollToken = UUID()
    var statusLine: String = ""
    var banner: String = ""
    var sendInFlight = false
    private(set) var sseRunning = false

    /// Rows from last `GET /api/chat/threads` (for picker UI).
    struct ThreadRow: Identifiable, Hashable {
        let id: String
        let title: String
        let subtitle: String
    }

    var threadRows: [ThreadRow] = []
    var threadsLoading = false
    var threadsBanner: String = ""

    /// Active conversation for send + SSE filtering (`nil` until threads load or user picks one).
    private(set) var currentThreadId: String?
    private var sseTask: Task<Void, Never>?

    func onAppear() {
        HubProfileSync.migrateGatewayAuthTokenFromUserDefaultsIfNeeded()
        restartSse()
        Task { await refreshThreads() }
    }

    func onDisappear() {
        sseTask?.cancel()
        sseTask = nil
        sseRunning = false
    }

    func restartSse() {
        sseTask?.cancel()
        sseTask = nil
        sseRunning = false

        let rawBase = UserDefaults.standard.string(forKey: HubProfileSync.Gateway.userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let base = HubProfileSync.normalizedGatewayBaseURL(rawBase)
        guard !base.isEmpty else {
            banner = "Set IronClaw gateway base URL in About (e.g. http://host:3000)."
            return
        }
        banner = ""

        let bearer = HubProfileSync.resolvedGatewayBearerToken()
        sseTask = Task { [base, bearer] in
            await self.runSseLoop(baseURL: base, bearer: bearer)
        }
    }

    func ensureThread(baseURL: String, bearer: String) async throws {
        if currentThreadId != nil { return }
        try await applyThreadListAndPickDefault(
            try await IronclawGatewayHTTP.listThreads(baseURL: baseURL, bearer: bearer),
            baseURL: baseURL,
            bearer: bearer
        )
    }

    /// Reload threads from the gateway and refresh `threadRows`. If `currentThreadId` is still `nil`, picks server default / first / assistant / creates one.
    func refreshThreads() async {
        let rawBase = UserDefaults.standard.string(forKey: HubProfileSync.Gateway.userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let base = HubProfileSync.normalizedGatewayBaseURL(rawBase)
        guard !base.isEmpty else {
            threadsBanner = "Set gateway URL in About to list threads."
            threadRows = []
            return
        }
        let bearer = HubProfileSync.resolvedGatewayBearerToken()
        threadsLoading = true
        threadsBanner = ""
        defer { threadsLoading = false }

        do {
            let list = try await IronclawGatewayHTTP.listThreads(baseURL: base, bearer: bearer)
            try await applyThreadListAndPickDefault(list, baseURL: base, bearer: bearer)
        } catch let e as GatewayHTTPError {
            threadsBanner = e.localizedDescription
            JarvisIOSLog.recordIronclawError("gateway list threads: \(e.localizedDescription)")
        } catch {
            threadsBanner = error.localizedDescription
            JarvisIOSLog.recordIronclawError("gateway list threads: \(error.localizedDescription)")
        }
    }

    /// Switch active thread and clear on-screen transcript (SSE stays global; events filter by `currentThreadId`).
    func selectThread(id: String) {
        guard !id.isEmpty else { return }
        currentThreadId = id
        lines = []
        liveAssistant = ""
        banner = ""
        statusLine = "Thread: \(shortThreadLabel(id))"
        Task { await self.loadHistoryForThread(id: id) }
    }

    /// Fetches prior turns from `GET /api/chat/history` (same as desktop `load_history_inner`).
    private func loadHistoryForThread(id: String) async {
        let rawBase = UserDefaults.standard.string(forKey: HubProfileSync.Gateway.userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let base = HubProfileSync.normalizedGatewayBaseURL(rawBase)
        guard !base.isEmpty, !id.isEmpty else { return }
        let bearer = HubProfileSync.resolvedGatewayBearerToken()
        let limit: UInt32 = 80
        do {
            let h = try await IronclawGatewayHTTP.fetchHistory(
                baseURL: base,
                bearer: bearer,
                threadId: id,
                limit: limit
            )
            let turns = h.turns ?? []
            var newLines: [ChatLine] = []
            newLines.reserveCapacity(turns.count * 2)
            for turn in turns {
                let u = turn.userInput.trimmingCharacters(in: .whitespacesAndNewlines)
                if !u.isEmpty {
                    newLines.append(ChatLine(role: .user, text: u))
                }
                let think = (turn.thinking ?? turn.reasoning)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                if !think.isEmpty {
                    newLines.append(ChatLine(role: .system, text: think))
                }
                if let raw = turn.response {
                    let r = raw.trimmingCharacters(in: .whitespacesAndNewlines)
                    if !r.isEmpty {
                        newLines.append(ChatLine(role: .assistant, text: r))
                    }
                }
            }
            guard currentThreadId == id else { return }
            lines = newLines
        } catch let e as GatewayHTTPError {
            banner = e.localizedDescription
            JarvisIOSLog.recordIronclawError("gateway history: \(e.localizedDescription)")
        } catch {
            banner = error.localizedDescription
            JarvisIOSLog.recordIronclawError("gateway history: \(error.localizedDescription)")
        }
    }

    /// Create a new thread on the gateway and select it. Returns whether the gateway accepted the request.
    @discardableResult
    func createNewThread() async -> Bool {
        let rawBase = UserDefaults.standard.string(forKey: HubProfileSync.Gateway.userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let base = HubProfileSync.normalizedGatewayBaseURL(rawBase)
        guard !base.isEmpty else {
            threadsBanner = "Set gateway URL in About first."
            return false
        }
        let bearer = HubProfileSync.resolvedGatewayBearerToken()
        threadsLoading = true
        threadsBanner = ""
        defer { threadsLoading = false }
        do {
            let created = try await IronclawGatewayHTTP.createThread(baseURL: base, bearer: bearer)
            selectThread(id: created.id)
            let list = try await IronclawGatewayHTTP.listThreads(baseURL: base, bearer: bearer)
            updateThreadRows(from: list)
            JarvisIOSLog.recordIronclaw("gateway POST /api/chat/thread/new id=\(created.id)")
            return true
        } catch let e as GatewayHTTPError {
            threadsBanner = e.localizedDescription
            return false
        } catch {
            threadsBanner = error.localizedDescription
            return false
        }
    }

    private func shortThreadLabel(_ id: String) -> String {
        if id.count <= 12 { return id }
        return String(id.prefix(8)) + "…"
    }

    private func orderedThreads(from list: ThreadListDTO) -> [ThreadInfoDTO] {
        var seen = Set<String>()
        var out: [ThreadInfoDTO] = []
        for t in list.threadsNonEmpty {
            guard seen.insert(t.id).inserted else { continue }
            out.append(t)
        }
        if let at = list.assistantThread, !seen.contains(at.id) {
            out.insert(at, at: 0)
        }
        return out
    }

    private func mapThreadRows(from list: ThreadListDTO) -> [ThreadRow] {
        orderedThreads(from: list).map { t in
            let title = (t.title?.trimmingCharacters(in: .whitespacesAndNewlines)).flatMap { $0.isEmpty ? nil : $0 }
                ?? shortThreadLabel(t.id)
            let sub = "Updated \(t.updatedAt) · \(t.turnCount) turns · \(t.state)"
            return ThreadRow(id: t.id, title: title, subtitle: sub)
        }
    }

    private func updateThreadRows(from list: ThreadListDTO) {
        threadRows = mapThreadRows(from: list)
    }

    /// If `currentThreadId` is still `nil`, pick server default / first / assistant / create one.
    private func pickDefaultThreadIfNeeded(_ list: ThreadListDTO, baseURL: String, bearer: String) async throws {
        if currentThreadId != nil { return }

        if let id = list.activeThread, !id.isEmpty {
            currentThreadId = id
            return
        }
        if let t = list.threadsNonEmpty.first {
            currentThreadId = t.id
            return
        }
        if let at = list.assistantThread {
            currentThreadId = at.id
            return
        }
        let created = try await IronclawGatewayHTTP.createThread(baseURL: baseURL, bearer: bearer)
        currentThreadId = created.id
        updateThreadRows(
            from: ThreadListDTO(assistantThread: nil, threads: [created], activeThread: created.id)
        )
    }

    /// Updates `threadRows`, assigns `currentThreadId` when it is still `nil`, then loads transcript from `/api/chat/history`.
    private func applyThreadListAndPickDefault(_ list: ThreadListDTO, baseURL: String, bearer: String) async throws {
        updateThreadRows(from: list)
        try await pickDefaultThreadIfNeeded(list, baseURL: baseURL, bearer: bearer)
        if let tid = currentThreadId, !tid.isEmpty {
            await loadHistoryForThread(id: tid)
        }
    }

    func send() async {
        let rawBase = UserDefaults.standard.string(forKey: HubProfileSync.Gateway.userDefaultsBaseURLKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let base = HubProfileSync.normalizedGatewayBaseURL(rawBase)
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !base.isEmpty, !text.isEmpty || !pendingImages.isEmpty else { return }

        let bearer = HubProfileSync.resolvedGatewayBearerToken()
        sendInFlight = true
        banner = ""
        defer { sendInFlight = false }

        do {
            try await ensureThread(baseURL: base, bearer: bearer)
            guard let tid = currentThreadId else {
                banner = "No thread id — check gateway /api/chat/threads"
                return
            }
            let wireImages: [ImageDataDTO] = pendingImages.map {
                ImageDataDTO(mediaType: $0.mediaType, data: $0.data.base64EncodedString())
            }
            let displayImages: [ChatLine.InlineImage] = pendingImages.map {
                ChatLine.InlineImage(mediaType: $0.mediaType, data: $0.data)
            }
            lines.append(ChatLine(role: .user, text: text, images: displayImages))
            draft = ""
            pendingImages.removeAll()
            liveAssistant = ""

            _ = try await IronclawGatewayHTTP.sendMessage(
                baseURL: base,
                bearer: bearer,
                content: text,
                threadId: tid,
                images: wireImages
            )
            JarvisIOSLog.recordIronclaw("gateway POST /api/chat/send ok thread=\(tid) images=\(wireImages.count)")
        } catch let e as GatewayHTTPError {
            banner = e.localizedDescription
            JarvisIOSLog.recordIronclawError("gateway send: \(e.localizedDescription)")
        } catch {
            banner = error.localizedDescription
            JarvisIOSLog.recordIronclawError("gateway send: \(error.localizedDescription)")
        }
    }

    func removePendingImage(id: UUID) {
        pendingImages.removeAll { $0.id == id }
    }

    /// Loads `PhotosPickerItem` payloads, downscales large bitmaps, and appends to `pendingImages`.
    func ingestPhotosPickerItems(_ items: [PhotosPickerItem]) async {
        for item in items.prefix(6) {
            guard let raw = try? await item.loadTransferable(type: Data.self) else { continue }
            let mime = Self.mimeFromMagicBytes(raw)
            let out = Self.downscaleForGateway(data: raw, preferredMime: mime)
            pendingImages.append(PendingImage(id: UUID(), mediaType: out.mime, data: out.data))
        }
    }

    private static func mimeFromMagicBytes(_ d: Data) -> String {
        guard d.count >= 4 else { return "application/octet-stream" }
        if d[0] == 0xFF, d[1] == 0xD8 { return "image/jpeg" }
        if d[0] == 0x89, d[1] == 0x50, d[2] == 0x4E, d[3] == 0x47 { return "image/png" }
        return "image/png"
    }

    private static func downscaleForGateway(data: Data, preferredMime: String) -> (mime: String, data: Data) {
        guard let img = UIImage(data: data) else { return (preferredMime, data) }
        let maxDim: CGFloat = 1600
        let w = img.size.width
        let h = img.size.height
        guard w > 0, h > 0 else { return (preferredMime, data) }
        let longest = max(w, h)
        guard longest > maxDim else { return (preferredMime, data) }
        let scale = maxDim / longest
        let nw = max(1, Int((w * scale).rounded()))
        let nh = max(1, Int((h * scale).rounded()))
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: nw, height: nh), format: format)
        let resized = renderer.image { _ in
            img.draw(in: CGRect(x: 0, y: 0, width: nw, height: nh))
        }
        guard let jpeg = resized.jpegData(compressionQuality: 0.86) else { return (preferredMime, data) }
        return ("image/jpeg", jpeg)
    }

    private func runSseLoop(baseURL: String, bearer: String) async {
        var authBackoff = false
        while !Task.isCancelled {
            if authBackoff {
                try? await Task.sleep(nanoseconds: 60 * 1_000_000_000)
                if Task.isCancelled { break }
                authBackoff = false
            }

            await MainActor.run { self.sseRunning = true; self.statusLine = "SSE connecting…" }

            let req: URLRequest
            do {
                req = try IronclawGatewayHTTP.openEventsRequest(baseURL: baseURL, bearer: bearer)
            } catch {
                await MainActor.run {
                    self.statusLine = "SSE URL error"
                    self.banner = error.localizedDescription
                    self.sseRunning = false
                }
                try? await Task.sleep(nanoseconds: 3 * 1_000_000_000)
                continue
            }

            do {
                let (bytes, resp) = try await URLSession.shared.bytes(for: req)
                if let http = resp as? HTTPURLResponse {
                    if http.statusCode == 401 || http.statusCode == 403 {
                        await MainActor.run {
                            self.statusLine = "SSE auth failed"
                            self.banner = "Gateway rejected token on SSE (HTTP \(http.statusCode))."
                            self.sseRunning = false
                        }
                        authBackoff = true
                        continue
                    }
                    guard (200 ... 299).contains(http.statusCode) else {
                        let hint502 =
                            http.statusCode == 502
                            ? " Reverse proxy could not reach IronClaw (confirm upstream is up; for nginx SSE use `proxy_buffering off` and a long `proxy_read_timeout`)."
                            : ""
                        await MainActor.run {
                            self.statusLine = "SSE HTTP \(http.statusCode)"
                            self.banner = "Gateway returned HTTP \(http.statusCode) for /api/chat/events.\(hint502)"
                            self.sseRunning = false
                        }
                        try? await Task.sleep(nanoseconds: 3 * 1_000_000_000)
                        continue
                    }
                }

                await MainActor.run { self.statusLine = "SSE connected"; self.sseRunning = true }

                var sseLines: [String] = []
                for try await line in bytes.lines {
                    if Task.isCancelled { break }

                    if line.isEmpty {
                        if !sseLines.isEmpty {
                            let payload = sseLines.joined(separator: "\n")
                            sseLines.removeAll()
                            if let ev = ParsedGatewayEvent.parse(jsonLine: payload) {
                                await MainActor.run {
                                    self.apply(ev, activeThread: self.currentThreadId)
                                }
                            }
                        }
                        continue
                    }
                    if line.hasPrefix(":") { continue }
                    if line.hasPrefix("data:") {
                        let rest = line.dropFirst(5).trimmingCharacters(in: .whitespaces)
                        sseLines.append(String(rest))
                    }
                }
            } catch {
                if Task.isCancelled { break }
                await MainActor.run {
                    self.statusLine = "SSE disconnected"
                    self.sseRunning = false
                }
                JarvisIOSLog.recordIronclawError("gateway SSE: \(error.localizedDescription)")
                try? await Task.sleep(nanoseconds: 2 * 1_000_000_000)
            }

            await MainActor.run { self.sseRunning = false }
        }
        await MainActor.run { self.sseRunning = false; self.statusLine = "SSE stopped" }
    }

    private func apply(_ ev: ParsedGatewayEvent, activeThread: String?) {
        func matchesThread(_ tid: String?) -> Bool {
            guard let active = activeThread, !active.isEmpty else { return true }
            guard let t = tid, !t.isEmpty else { return true }
            return t == active
        }

        switch ev {
        case .response(let content, let tid):
            guard matchesThread(tid) else { return }
            liveAssistant = ""
            if !content.isEmpty {
                lines.append(ChatLine(role: .assistant, text: content))
            }
        case .streamChunk(let content, let tid):
            guard matchesThread(tid) else { return }
            liveAssistant += content
        case .thinking(let message):
            statusLine = message
        case .status(let message):
            statusLine = message
        case .toolStarted(let name, let detail):
            let d = detail.map { " — \($0)" } ?? ""
            lines.append(ChatLine(role: .system, text: "Tool: \(name)\(d)"))
        case .toolCompleted(let name, let success, let err):
            let e = err.map { " (\($0))" } ?? ""
            lines.append(ChatLine(role: .system, text: "Tool done: \(name) ok=\(success)\(e)"))
        case .toolResult(let name, let preview):
            lines.append(ChatLine(role: .system, text: "Result \(name): \(preview)"))
        case .error(let message):
            banner = message
        case .other(let typ):
            JarvisIOSLog.recordIronclaw("gateway SSE type=\(typ)")
        }
    }
}

// MARK: - View

struct GatewayChatView: View {
    @Bindable var model: GatewayChatViewModel
    /// Bottom sheet over the avatar: tighter chrome, optional dismiss control.
    var compact: Bool = false
    var onDismissCompact: (() -> Void)? = nil

    @State private var showThreadPicker = false
    @State private var photoPickerItems: [PhotosPickerItem] = []
    @AppStorage(HubProfileSync.Gateway.userDefaultsBaseURLKey) private var gatewayBaseURL: String = ""
    @AppStorage(HubProfileSync.Gateway.userDefaultsAuthTokenKey) private var gatewayAuthToken: String = ""

    private var canSend: Bool {
        let t = model.draft.trimmingCharacters(in: .whitespacesAndNewlines)
        return !model.sendInFlight && (!t.isEmpty || !model.pendingImages.isEmpty)
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if !model.banner.isEmpty {
                    Text(model.banner)
                        .font(compact ? .caption2 : .caption)
                        .foregroundStyle(.red)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                        .background(Color.red.opacity(0.12))
                }
                if !model.threadsBanner.isEmpty {
                    Text(model.threadsBanner)
                        .font(.caption2)
                        .foregroundStyle(.orange)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .background(Color.orange.opacity(0.12))
                }
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 10) {
                            ForEach(model.lines) { line in
                                bubble(line)
                                    .id(line.id)
                            }
                            if !model.liveAssistant.isEmpty {
                                bubble(GatewayChatViewModel.ChatLine(role: .assistant, text: model.liveAssistant))
                                    .id(model.liveScrollToken)
                            }
                        }
                        .padding(compact ? 8 : 16)
                    }
                    .onChange(of: model.lines.count) { _, _ in
                        if let last = model.lines.last {
                            withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                        }
                    }
                    .onChange(of: model.liveAssistant) { _, _ in
                        withAnimation { proxy.scrollTo(model.liveScrollToken, anchor: .bottom) }
                    }
                }
                Divider()
                if !model.pendingImages.isEmpty {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(model.pendingImages) { p in
                                ZStack(alignment: .topTrailing) {
                                    if let ui = UIImage(data: p.data) {
                                        Image(uiImage: ui)
                                            .resizable()
                                            .scaledToFill()
                                            .frame(width: compact ? 52 : 64, height: compact ? 52 : 64)
                                            .clipped()
                                            .clipShape(RoundedRectangle(cornerRadius: 8))
                                    }
                                    Button {
                                        model.removePendingImage(id: p.id)
                                    } label: {
                                        Image(systemName: "xmark.circle.fill")
                                            .symbolRenderingMode(.palette)
                                            .foregroundStyle(.white, .black.opacity(0.55))
                                            .font(.caption)
                                    }
                                    .offset(x: 4, y: -4)
                                }
                            }
                        }
                        .padding(.horizontal, compact ? 8 : 16)
                        .padding(.vertical, 6)
                    }
                }
                HStack(alignment: .bottom, spacing: 8) {
                    PhotosPicker(selection: $photoPickerItems, maxSelectionCount: 6, matching: .images) {
                        Image(systemName: "photo.on.rectangle.angled")
                            .font(compact ? .body : .title3)
                    }
                    .accessibilityLabel("Attach images")
                    .onChange(of: photoPickerItems) { _, newItems in
                        guard !newItems.isEmpty else { return }
                        let batch = newItems
                        photoPickerItems.removeAll()
                        Task { await model.ingestPhotosPickerItems(batch) }
                    }
                    TextField("Message", text: $model.draft, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .lineLimit(1 ... (compact ? 4 : 6))
                    Button {
                        Task { await model.send() }
                    } label: {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(compact ? .title3 : .title2)
                    }
                    .disabled(!canSend)
                }
                .padding(compact ? 8 : 16)
            }
            .background(compact ? Color.clear : Color(uiColor: .systemGroupedBackground))
            .navigationTitle("Chat")
            .navigationBarTitleDisplayMode(compact ? .inline : .large)
            .toolbar {
                if compact, let dismiss = onDismissCompact {
                    ToolbarItem(placement: .topBarLeading) {
                        Button {
                            dismiss()
                        } label: {
                            Label("Hide overlay", systemImage: "chevron.down.circle.fill")
                        }
                    }
                }
                if !compact {
                    ToolbarItem(placement: .topBarLeading) {
                        Button {
                            showThreadPicker = true
                        } label: {
                            Label("Threads", systemImage: "list.bullet.rectangle")
                        }
                        .accessibilityHint("Choose IronClaw chat thread")
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    HStack(spacing: 12) {
                        if compact {
                            Button {
                                showThreadPicker = true
                            } label: {
                                Image(systemName: "list.bullet.rectangle")
                            }
                            .accessibilityLabel("Threads")
                        }
                        Circle()
                            .fill(model.sseRunning ? Color.green : Color.orange.opacity(0.6))
                            .frame(width: 10, height: 10)
                            .accessibilityLabel(model.sseRunning ? "SSE connected" : "SSE reconnecting")
                    }
                }
            }
            .sheet(isPresented: $showThreadPicker) {
                GatewayThreadPickerSheet(model: model) {
                    showThreadPicker = false
                }
            }
            .safeAreaInset(edge: .top) {
                if !model.statusLine.isEmpty {
                    Text(model.statusLine)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal)
                        .padding(.vertical, 4)
                }
            }
            .onChange(of: gatewayBaseURL) { _, _ in
                model.restartSse()
                Task { await model.refreshThreads() }
            }
            .onChange(of: gatewayAuthToken) { _, _ in
                model.restartSse()
                Task { await model.refreshThreads() }
            }
        }
    }

    @ViewBuilder
    private func bubble(_ line: GatewayChatViewModel.ChatLine) -> some View {
        let maxImg: CGFloat = compact ? 160 : 240
        switch line.role {
        case .user:
            VStack(alignment: .trailing, spacing: 8) {
                if !line.text.isEmpty {
                    Text(line.text)
                        .padding(10)
                        .background(Color.accentColor.opacity(0.2))
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                }
                if !line.images.isEmpty {
                    ForEach(line.images) { img in
                        if let ui = UIImage(data: img.data) {
                            Image(uiImage: ui)
                                .resizable()
                                .scaledToFit()
                                .frame(maxWidth: maxImg, maxHeight: maxImg)
                                .clipShape(RoundedRectangle(cornerRadius: 10))
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
        case .assistant:
            VStack(alignment: .leading, spacing: 8) {
                if !line.text.isEmpty {
                    Text(line.text)
                        .padding(10)
                        .background(Color(uiColor: .secondarySystemBackground))
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                }
                if !line.images.isEmpty {
                    ForEach(line.images) { img in
                        if let ui = UIImage(data: img.data) {
                            Image(uiImage: ui)
                                .resizable()
                                .scaledToFit()
                                .frame(maxWidth: maxImg, maxHeight: maxImg)
                                .clipShape(RoundedRectangle(cornerRadius: 10))
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        case .system:
            Text(line.text)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

// MARK: - Thread picker (IronClaw gateway)

private struct GatewayThreadPickerSheet: View {
    @Bindable var model: GatewayChatViewModel
    var onDismiss: () -> Void

    var body: some View {
        NavigationStack {
            Group {
                if model.threadsLoading && model.threadRows.isEmpty {
                    ProgressView("Loading threads…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if model.threadRows.isEmpty {
                    VStack(spacing: 16) {
                        if !model.threadsBanner.isEmpty {
                            Text(model.threadsBanner)
                                .font(.caption)
                                .foregroundStyle(.red)
                                .multilineTextAlignment(.center)
                                .padding(.horizontal)
                        }
                        ContentUnavailableView(
                            "No threads",
                            systemImage: "bubble.left.and.bubble.right",
                            description: Text("Pull to refresh, or create a new thread.")
                        )
                    }
                } else {
                    List {
                        if !model.threadsBanner.isEmpty {
                            Section {
                                Text(model.threadsBanner)
                                    .font(.caption)
                                    .foregroundStyle(.red)
                            }
                        }
                        ForEach(model.threadRows) { row in
                            Button {
                                model.selectThread(id: row.id)
                                onDismiss()
                            } label: {
                                HStack(alignment: .top, spacing: 12) {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(row.title)
                                            .font(.body)
                                            .foregroundStyle(.primary)
                                        Text(row.subtitle)
                                            .font(.caption2)
                                            .foregroundStyle(.secondary)
                                            .multilineTextAlignment(.leading)
                                    }
                                    Spacer(minLength: 8)
                                    if model.currentThreadId == row.id {
                                        Image(systemName: "checkmark.circle.fill")
                                            .foregroundStyle(.tint)
                                            .imageScale(.medium)
                                    }
                                }
                                .padding(.vertical, 4)
                            }
                        }
                    }
                    .listStyle(.insetGrouped)
                }
            }
            .navigationTitle("Threads")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { onDismiss() }
                }
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        Task { @MainActor in
                            if await model.createNewThread() {
                                onDismiss()
                            }
                        }
                    } label: {
                        Label("New thread", systemImage: "plus.circle.fill")
                    }
                    .disabled(model.threadsLoading)
                }
            }
            .refreshable { await model.refreshThreads() }
            .task { await model.refreshThreads() }
        }
    }
}
