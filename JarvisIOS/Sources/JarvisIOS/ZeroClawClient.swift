// ZeroClawClient.swift
//
// Swift port of `src/zeroclaw/client.rs` + `src/zeroclaw/types.rs`. Covers
// the chat surface only — HTTP `/webhook`, WS `/ws/chat`, plus the
// session-management endpoints (`/api/sessions`, `/api/sessions/{id}/messages`)
// the chat view uses to populate the sidebar.
//
// Two intentional omissions vs desktop:
//   * `/api/events` SSE — left out because the iOS chat view consumes
//     completion only; the desktop's tool-event correlation isn't relevant
//     when the chat tab is the entire surface. Easy to add later by mirroring
//     `IronclawGatewayHTTP.connectSSE` in this file.
//   * `/api/memory` — desktop's `zeroclaw_context` pushes avatar state; iOS
//     has no Bevy avatar of its own to report.

import Foundation

// MARK: - DTOs (wire shapes match Rust types.rs exactly)

struct ZCWebhookRequest: Encodable {
    let message: String
}

struct ZCWebhookResponse: Decodable {
    let response: String?
    let model: String?
    let status: String?
    let idempotent: Bool?
}

struct ZCSessionInfo: Decodable, Identifiable {
    let sessionId: String
    let sessionKey: String?
    let createdAt: String?
    let lastActivity: String?
    let messageCount: Int?
    let agentAlias: String?
    let channelId: String?
    let name: String?

    var id: String { sessionId }
}

struct ZCSessionListResponse: Decodable {
    let sessions: [ZCSessionInfo]?
}

struct ZCSessionMessage: Decodable {
    let role: String
    let content: String
    let createdAt: String?
}

struct ZCSessionMessagesResponse: Decodable {
    let sessionId: String?
    let messages: [ZCSessionMessage]?
    let sessionPersistence: Bool?
}

/// WS frame the client sends. Mirrors `WsClientMessage` in Rust.
struct ZCWsClientMessage: Encodable {
    let type: String
    let content: String

    init(content: String) {
        self.type = "message"
        self.content = content
    }
}

/// WS frame the server sends. Mirrors `WsServerMessage`.
enum ZCWsServerMessage {
    case done(String)
    case delta(String)
    case error(String)
    case other
}

// MARK: - Errors

enum ZeroClawError: LocalizedError {
    case badURL
    case badStatus(Int, String)
    case authRejected
    case wsClosedBeforeReply
    case wsHandshake(String)
    case decode(String)

    var errorDescription: String? {
        switch self {
        case .badURL: return "Invalid ZeroClaw URL"
        case .badStatus(let code, let body): return "HTTP \(code): \(body.prefix(200))"
        case .authRejected: return "ZeroClaw rejected the bearer token (401/403)"
        case .wsClosedBeforeReply: return "ZeroClaw WS closed before sending a reply"
        case .wsHandshake(let msg): return "ZeroClaw WS handshake failed: \(msg)"
        case .decode(let msg): return "ZeroClaw decode failed: \(msg)"
        }
    }
}

// MARK: - HTTP / WS client

/// Async HTTP + WS client. Stateless; instantiate per call site or hold one
/// on a view model. Reads its config from `ZeroClawSettings` so the same
/// instance picks up the latest user-edited URL/token without rewiring.
struct ZeroClawClient {
    let baseURL: String
    let wsURL: String
    let bearer: String
    let webhookSecret: String
    let agentAlias: String
    let clientId: String

    init(
        baseURL: String? = nil,
        wsURL: String? = nil,
        bearer: String? = nil,
        webhookSecret: String? = nil,
        agentAlias: String? = nil,
        clientId: String? = nil
    ) {
        self.baseURL = (baseURL ?? ZeroClawSettings.normalizedBaseURL)
        self.wsURL = (wsURL ?? ZeroClawSettings.resolvedWSURL)
        self.bearer = (bearer ?? ZeroClawSettings.authToken)
        self.webhookSecret = (webhookSecret ?? ZeroClawSettings.webhookSecret)
        self.agentAlias = (agentAlias ?? ZeroClawSettings.agentAlias)
        self.clientId = (clientId ?? ZeroClawSettings.clientId)
    }

    private func decoder() -> JSONDecoder {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }

    private func encoder() -> JSONEncoder {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }

    private func attachHeaders(_ req: inout URLRequest) {
        if !bearer.isEmpty {
            req.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        }
        if !clientId.isEmpty {
            req.setValue(clientId, forHTTPHeaderField: "X-Client")
            req.setValue("\(clientId)/0.1", forHTTPHeaderField: "User-Agent")
        }
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        req.setValue("identity", forHTTPHeaderField: "Accept-Encoding")
    }

    private func decodeResponse<T: Decodable>(_ data: Data, _ resp: URLResponse, as type: T.Type) throws -> T {
        try throwIfNonOK(resp, data)
        do {
            return try decoder().decode(type, from: data)
        } catch {
            throw ZeroClawError.decode(error.localizedDescription)
        }
    }

    private func throwIfNonOK(_ resp: URLResponse, _ data: Data) throws {
        guard let http = resp as? HTTPURLResponse else { return }
        if http.statusCode == 401 || http.statusCode == 403 {
            throw ZeroClawError.authRejected
        }
        if !(200..<300).contains(http.statusCode) {
            let body = String(data: data, encoding: .utf8) ?? ""
            throw ZeroClawError.badStatus(http.statusCode, body)
        }
    }

    // ---- /health ------------------------------------------------------------

    func health() async -> Bool {
        guard let url = URL(string: baseURL + "/health") else { return false }
        var req = URLRequest(url: url)
        req.timeoutInterval = 8
        attachHeaders(&req)
        do {
            let (_, resp) = try await URLSession.shared.data(for: req)
            return (resp as? HTTPURLResponse).map { (200..<300).contains($0.statusCode) } ?? false
        } catch {
            return false
        }
    }

    // ---- POST /webhook ------------------------------------------------------

    func webhook(message: String, idempotencyKey: String? = nil) async throws -> ZCWebhookResponse {
        var comp = URLComponents(string: baseURL + "/webhook")
        comp?.queryItems = agentAlias.isEmpty ? nil : [URLQueryItem(name: "agent", value: agentAlias)]
        guard let url = comp?.url else { throw ZeroClawError.badURL }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        attachHeaders(&req)
        if !webhookSecret.isEmpty {
            req.setValue(webhookSecret, forHTTPHeaderField: "X-Webhook-Secret")
        }
        if let key = idempotencyKey, !key.isEmpty {
            req.setValue(key, forHTTPHeaderField: "X-Idempotency-Key")
        }
        req.httpBody = try encoder().encode(ZCWebhookRequest(message: message))
        req.timeoutInterval = 120
        let (data, resp) = try await URLSession.shared.data(for: req)
        return try decodeResponse(data, resp, as: ZCWebhookResponse.self)
    }

    // ---- GET /api/sessions --------------------------------------------------

    func listSessions() async throws -> [ZCSessionInfo] {
        guard let url = URL(string: baseURL + "/api/sessions") else { throw ZeroClawError.badURL }
        var req = URLRequest(url: url)
        attachHeaders(&req)
        req.timeoutInterval = 30
        let (data, resp) = try await URLSession.shared.data(for: req)
        let decoded = try decodeResponse(data, resp, as: ZCSessionListResponse.self)
        return decoded.sessions ?? []
    }

    func sessionMessages(sessionId: String) async throws -> [ZCSessionMessage] {
        guard let encoded = sessionId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed),
              let url = URL(string: baseURL + "/api/sessions/\(encoded)/messages") else {
            throw ZeroClawError.badURL
        }
        var req = URLRequest(url: url)
        attachHeaders(&req)
        req.timeoutInterval = 30
        let (data, resp) = try await URLSession.shared.data(for: req)
        let decoded = try decodeResponse(data, resp, as: ZCSessionMessagesResponse.self)
        return decoded.messages ?? []
    }

    func deleteSession(sessionId: String) async throws {
        guard let encoded = sessionId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed),
              let url = URL(string: baseURL + "/api/sessions/\(encoded)") else {
            throw ZeroClawError.badURL
        }
        var req = URLRequest(url: url)
        req.httpMethod = "DELETE"
        attachHeaders(&req)
        req.timeoutInterval = 30
        let (data, resp) = try await URLSession.shared.data(for: req)
        try throwIfNonOK(resp, data)
    }

    // ---- WS /ws/chat --------------------------------------------------------

    /// Build the WS URL with `?agent=<alias>[&session_id=<id>][&token=<bearer>]`.
    /// The gateway returns HTTP 400 if `agent` is missing.
    func buildChatWSURL(sessionId: String?) -> URL? {
        var comp = URLComponents(string: wsURL + "/ws/chat")
        var items: [URLQueryItem] = [URLQueryItem(name: "agent", value: agentAlias)]
        if let sid = sessionId, !sid.isEmpty {
            items.append(URLQueryItem(name: "session_id", value: sid))
        }
        if !bearer.isEmpty {
            items.append(URLQueryItem(name: "token", value: bearer))
        }
        comp?.queryItems = items
        return comp?.url
    }

    /// Open the WS, send a single message, await the `done` frame, close. Used
    /// when `prefer_streaming` is on. Returns `(reply, model)`; `model` is
    /// nil since the WS path doesn't carry it (the `/webhook` JSON does).
    func sendViaWS(message: String, sessionId: String?) async throws -> String {
        guard let url = buildChatWSURL(sessionId: sessionId) else { throw ZeroClawError.badURL }
        let task = URLSession.shared.webSocketTask(with: url)
        // URLSession's WS handshake doesn't easily surface upgrade errors —
        // a 400 lands as a generic "socket not connected" on the first send.
        // We optimistically start, then send, then await; failures bubble up.
        task.resume()
        defer { task.cancel(with: .goingAway, reason: nil) }

        let frame: String
        do {
            let body = try encoder().encode(ZCWsClientMessage(content: message))
            frame = String(data: body, encoding: .utf8) ?? "{}"
        } catch {
            throw ZeroClawError.decode(error.localizedDescription)
        }
        try await task.send(.string(frame))

        // Loop receiving until we see `done` or `error`. ZeroClaw 0.8 sends
        // a single `done` per turn; the loop tolerates future `delta`s.
        while true {
            let received: URLSessionWebSocketTask.Message
            do {
                received = try await task.receive()
            } catch {
                throw ZeroClawError.wsHandshake(error.localizedDescription)
            }
            let text: String
            switch received {
            case .string(let s):
                text = s
            case .data(let d):
                text = String(data: d, encoding: .utf8) ?? ""
            @unknown default:
                continue
            }
            guard let data = text.data(using: .utf8),
                  let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                continue
            }
            let kind = (value["type"] as? String) ?? ""
            switch kind {
            case "done":
                return (value["full_response"] as? String) ?? ""
            case "delta":
                // No-op for now; future per-token streaming would route here.
                continue
            case "error":
                throw ZeroClawError.wsHandshake((value["message"] as? String) ?? "ws error")
            default:
                continue
            }
        }
    }
}
