import Foundation

/// Owns the channel hub WebSocket (peer / heartbeat parity). Gateway chat uses HTTP/SSE separately.
@MainActor
final class IronclawConnectivity {
    static let shared = IronclawConnectivity()

    private let hubClient = JarvisHubWebSocketClient()
    private init() {}

    /// Call at launch (e.g. `ContentView.onAppear`) after hub env warm-up.
    func start() {
        HubProfileSync.migrateGatewayAuthTokenFromUserDefaultsIfNeeded()
        guard let wsURL = HubProfileSync.hubWebSocketURL() else {
            hubClient.stop()
            JarvisIOSLog.recordIronclaw("hub WS: skip (no hub base URL or invalid for ws)")
            return
        }
        let token = HubProfileSync.resolvedHubBearerToken()
        if token.isEmpty {
            hubClient.stop()
            JarvisIOSLog.recordIronclaw("hub WS: skip (empty hub bearer token)")
            return
        }
        hubClient.connect(webSocketURL: wsURL, hubBearerToken: token)
    }

    func stopHubWebSocket() {
        hubClient.stop()
    }
}
