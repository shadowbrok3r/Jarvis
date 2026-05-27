// ZeroClawSettings.swift
//
// Mirrors `Settings.zeroclaw` (Rust `src/config.rs::ZeroClawSettings`) for the
// iOS side. Persistence uses `UserDefaults` so the same settings survive app
// relaunches — matches how the iOS hub / Kokoro / IronClaw settings are
// already stored under `jarvis.*` keys.
//
// Scope is deliberately chat-only: the desktop's `zeroclaw_attachments` HTTP
// server (publishes images for the agent to fetch) and `zeroclaw_context`
// memory pusher (avatar pose / emotion / look-at into `/api/memory`) don't
// have iOS analogues, so they're skipped here.

import Foundation

enum ChatBackend: String, CaseIterable, Identifiable {
    case ironclaw = "ironclaw"
    case zeroclaw = "zeroclaw"

    var id: String { rawValue }

    var displayLabel: String {
        switch self {
        case .ironclaw: return "IronClaw"
        case .zeroclaw: return "ZeroClaw"
        }
    }

    /// Read the user's saved choice. Defaults to IronClaw so existing
    /// installs behave identically to before.
    static var current: ChatBackend {
        get {
            let raw = UserDefaults.standard.string(forKey: ZeroClawSettings.userDefaultsBackendKey) ?? "ironclaw"
            return ChatBackend(rawValue: raw) ?? .ironclaw
        }
        set {
            UserDefaults.standard.set(newValue.rawValue, forKey: ZeroClawSettings.userDefaultsBackendKey)
        }
    }
}

/// Namespace for ZeroClaw `UserDefaults` keys + derived helpers. Parallels
/// `HubProfileSync.Gateway` for the IronClaw side.
enum ZeroClawSettings {
    // ---- UserDefaults keys (single source of truth) -------------------------

    static let userDefaultsBackendKey = "jarvis.chat.backend"
    static let userDefaultsBaseURLKey = "jarvis.zeroclaw.baseURL"
    static let userDefaultsWSURLKey = "jarvis.zeroclaw.wsURL"
    static let userDefaultsAuthTokenKey = "jarvis.zeroclaw.authToken"
    static let userDefaultsWebhookSecretKey = "jarvis.zeroclaw.webhookSecret"
    static let userDefaultsAgentAliasKey = "jarvis.zeroclaw.agentAlias"
    static let userDefaultsClientIdKey = "jarvis.zeroclaw.clientId"
    static let userDefaultsPreferStreamingKey = "jarvis.zeroclaw.preferStreaming"
    static let userDefaultsActiveSessionIdKey = "jarvis.zeroclaw.activeSessionId"
    static let userDefaultsHistoryWindowKey = "jarvis.zeroclaw.historyWindow"
    static let userDefaultsSessionLimitKey = "jarvis.zeroclaw.sessionListLimit"
    static let userDefaultsMemoryCategoryKey = "jarvis.zeroclaw.memoryCategory"

    // ---- Defaults (match desktop `config/default.toml`) ---------------------

    static let defaultBaseURL = "https://claw.shadowbroker.app"
    static let defaultAgentAlias = "default"
    static let defaultClientId = "jarvis-ios"
    static let defaultHistoryWindow: Int = 6
    static let defaultSessionLimit: Int = 50
    static let defaultMemoryCategory = "jarvis-ios"

    // ---- Accessors ----------------------------------------------------------

    static var baseURL: String {
        get { trimmed(UserDefaults.standard.string(forKey: userDefaultsBaseURLKey)) ?? defaultBaseURL }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsBaseURLKey) }
    }

    static var wsURLOverride: String {
        get { trimmed(UserDefaults.standard.string(forKey: userDefaultsWSURLKey)) ?? "" }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsWSURLKey) }
    }

    static var authToken: String {
        get { trimmed(UserDefaults.standard.string(forKey: userDefaultsAuthTokenKey)) ?? "" }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsAuthTokenKey) }
    }

    static var webhookSecret: String {
        get { trimmed(UserDefaults.standard.string(forKey: userDefaultsWebhookSecretKey)) ?? "" }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsWebhookSecretKey) }
    }

    static var agentAlias: String {
        get {
            let v = trimmed(UserDefaults.standard.string(forKey: userDefaultsAgentAliasKey)) ?? ""
            return v.isEmpty ? defaultAgentAlias : v
        }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsAgentAliasKey) }
    }

    static var clientId: String {
        get {
            let v = trimmed(UserDefaults.standard.string(forKey: userDefaultsClientIdKey)) ?? ""
            return v.isEmpty ? defaultClientId : v
        }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsClientIdKey) }
    }

    static var preferStreaming: Bool {
        get {
            if UserDefaults.standard.object(forKey: userDefaultsPreferStreamingKey) == nil { return true }
            return UserDefaults.standard.bool(forKey: userDefaultsPreferStreamingKey)
        }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsPreferStreamingKey) }
    }

    static var activeSessionId: String {
        get { trimmed(UserDefaults.standard.string(forKey: userDefaultsActiveSessionIdKey)) ?? "" }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsActiveSessionIdKey) }
    }

    static var historyWindow: Int {
        get {
            let v = UserDefaults.standard.integer(forKey: userDefaultsHistoryWindowKey)
            return v <= 0 ? defaultHistoryWindow : v
        }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsHistoryWindowKey) }
    }

    static var sessionListLimit: Int {
        get {
            let v = UserDefaults.standard.integer(forKey: userDefaultsSessionLimitKey)
            return v <= 0 ? defaultSessionLimit : v
        }
        set { UserDefaults.standard.set(newValue, forKey: userDefaultsSessionLimitKey) }
    }

    // ---- Derived helpers ----------------------------------------------------

    /// `base_url` with trailing slash removed (used as `format!("{base}/path")`).
    static var normalizedBaseURL: String {
        let raw = baseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        if raw.hasSuffix("/") {
            return String(raw.dropLast())
        }
        return raw
    }

    /// Resolve the WebSocket base. Honours the user override; otherwise flips
    /// the scheme on `baseURL` (`http→ws`, `https→wss`). Parallels Rust
    /// `ZeroClawSettings::resolved_ws_url`.
    static var resolvedWSURL: String {
        let override = wsURLOverride
        if !override.isEmpty {
            return override.hasSuffix("/") ? String(override.dropLast()) : override
        }
        let base = normalizedBaseURL
        if base.hasPrefix("https://") {
            return "wss://" + base.dropFirst("https://".count)
        }
        if base.hasPrefix("http://") {
            return "ws://" + base.dropFirst("http://".count)
        }
        return "ws://" + base
    }

    private static func trimmed(_ s: String?) -> String? {
        guard let s = s else { return nil }
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        return t.isEmpty ? nil : t
    }
}
