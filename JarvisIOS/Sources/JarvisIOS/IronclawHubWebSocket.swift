import Foundation

/// Minimal jarvis-avatar channel hub (`ws://…:6121/ws`) client: `module:authenticate`, `module:announce`, heartbeat `pong`.
/// Chat text still comes from the **gateway** SSE; this socket is for peer envelopes / HA parity.
final class JarvisHubWebSocketClient: @unchecked Sendable {
    private var task: URLSessionWebSocketTask?
    private let session: URLSession
    private let moduleName: String
    private var candidateURLs: [URL] = []
    private var urlIndex: Int = 0
    private var bearerToken: String = ""

    init(moduleName: String = "jarvis-ios") {
        self.moduleName = moduleName
        let cfg = URLSessionConfiguration.default
        cfg.waitsForConnectivity = true
        self.session = URLSession(configuration: cfg)
    }

    func stop() {
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
    }

    /// Connect using the first reachable URL; on persistent receive failure, rotates through `candidates`.
    func connect(candidates: [URL], hubBearerToken: String) {
        stop()
        candidateURLs = candidates
        urlIndex = 0
        bearerToken = hubBearerToken
        guard let first = currentURL() else {
            JarvisIOSLog.recordIronclaw("hub WS: skip (no candidate URLs)")
            return
        }
        openSocket(to: first)
    }

    private func currentURL() -> URL? {
        guard !candidateURLs.isEmpty else { return nil }
        let i = urlIndex % candidateURLs.count
        return candidateURLs[i]
    }

    private func openSocket(to url: URL) {
        let t = session.webSocketTask(with: url)
        task = t
        JarvisIOSLog.recordIronclaw("hub WS: connecting \(url.absoluteString)")
        t.resume()
        sendEnvelope(type: "module:authenticate", data: ["token": bearerToken], source: moduleName)
        sendEnvelope(
            type: "module:announce",
            data: ["name": moduleName, "identity": [String: Any]()],
            source: moduleName
        )
        receiveLoop()
    }

    private func tryNextEndpointAfterFailure() {
        guard candidateURLs.count > 1 else { return }
        urlIndex += 1
        stop()
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
            guard let self, let u = self.currentURL() else { return }
            JarvisIOSLog.recordIronclaw("hub WS: failover → \(u.absoluteString)")
            self.openSocket(to: u)
        }
    }

    private func receiveLoop() {
        task?.receive { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(let message):
                if case .string(let text) = message {
                    self.handleIncoming(text)
                } else if case .data(let data) = message, let text = String(data: data, encoding: .utf8) {
                    self.handleIncoming(text)
                }
                self.receiveLoop()
            case .failure(let err):
                JarvisIOSLog.recordIronclawError("hub WS receive: \(err.localizedDescription)")
                self.tryNextEndpointAfterFailure()
            }
        }
    }

    private func handleIncoming(_ text: String) {
        guard let data = text.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            JarvisIOSLog.recordIronclaw("hub WS (non-JSON): \(text.prefix(200))")
            return
        }
        let envelope = (obj["json"] as? [String: Any]) ?? obj
        let typ = envelope["type"] as? String ?? "(no type)"
        if typ == "transport:connection:heartbeat",
           let d = envelope["data"] as? [String: Any],
           (d["kind"] as? String) == "ping"
        {
            let ts = Int64(Date().timeIntervalSince1970 * 1000)
            sendEnvelope(
                type: "transport:connection:heartbeat",
                data: ["kind": "pong", "timestamp": ts],
                source: moduleName
            )
            return
        }
        if typ == "module:authenticated" || typ == "module:announced" {
            JarvisIOSLog.recordIronclaw("hub WS: \(typ)")
            return
        }
        if typ == "error",
           let d = envelope["data"] as? [String: Any],
           let code = d["code"] as? String
        {
            JarvisIOSLog.recordIronclawError("hub WS error: \(code) \(d["message"] as? String ?? "")")
            return
        }
        JarvisIOSLog.recordIronclaw("hub WS ← type=\(typ)")
    }

    private func sendEnvelope(type: String, data: [String: Any], source: String) {
        let id = UUID().uuidString
        let metadata: [String: Any] = [
            "event": ["id": id],
            "source": ["kind": "module", "id": source],
        ]
        let root: [String: Any] = ["type": type, "data": data, "metadata": metadata]
        guard let body = try? JSONSerialization.data(withJSONObject: root),
              let str = String(data: body, encoding: .utf8)
        else { return }
        task?.send(.string(str)) { err in
            if let err {
                JarvisIOSLog.recordIronclawError("hub WS send \(type): \(err.localizedDescription)")
            }
        }
    }
}
