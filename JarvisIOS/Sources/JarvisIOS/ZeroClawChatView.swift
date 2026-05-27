// ZeroClawChatView.swift
//
// Swift parallel to the desktop `src/plugins/zeroclaw_chat.rs` Bevy plugin.
// Owns:
//   * A `ZeroClawClient` for HTTP + WS calls.
//   * A persistent active-session id (UserDefaults, parallels desktop
//     `[zeroclaw].active_session_id`).
//   * A rolling client-side history fallback for stateless `/webhook` use.
//   * The chat sidebar populated from `GET /api/sessions` so prior
//     conversations show up after a relaunch, just like the desktop.
//
// Kept lean by reusing `IosChatFormatting` (ACT/DELAY strip + emotion
// extraction) and `HubProfileSync.mappedEmotionKeysLowercased()`.

import SwiftUI
import UIKit

@Observable @MainActor
final class ZeroClawChatViewModel {
    /// Bubble model — same shape as `GatewayChatViewModel.ChatLine` so the
    /// UI rendering helpers can be lifted/shared later if it becomes
    /// worthwhile.
    struct ChatLine: Identifiable {
        enum Role { case user, assistant, system }
        let id: UUID
        let role: Role
        var text: String
        var emotionCaption: String?
        var emotionUnmapped: Bool

        init(role: Role, text: String, emotionCaption: String? = nil, emotionUnmapped: Bool = false) {
            self.id = UUID()
            self.role = role
            self.text = text
            self.emotionCaption = emotionCaption
            self.emotionUnmapped = emotionUnmapped
        }
    }

    /// Display row in the session sidebar. ZeroClaw calls them "sessions"
    /// internally; we surface them under the existing "Threads" UI naming
    /// so the user lands on familiar ground.
    struct SessionRow: Identifiable {
        let id: String  // session_id
        let title: String
        let subtitle: String
        let messageCount: Int
        let isActive: Bool
    }

    // ---- Observable state ---------------------------------------------------

    var lines: [ChatLine] = []
    var sessions: [SessionRow] = []
    var input: String = ""
    var statusLine: String = "idle"
    var inFlight: Bool = false
    var activeSessionId: String = ""
    /// Last error surfaced to the UI; cleared on the next successful op.
    var lastError: String?

    // ---- Private state ------------------------------------------------------

    private var history: [(ChatLine.Role, String)] = []  // rolling window for /webhook fallback context

    init() {
        let saved = ZeroClawSettings.activeSessionId
        if saved.isEmpty {
            // Mint a session id up front; we'll persist it on first
            // successful send (matches desktop behaviour).
            self.activeSessionId = UUID().uuidString
        } else {
            self.activeSessionId = saved
        }
    }

    // ---- Bootstrapping ------------------------------------------------------

    /// Pull the session list and (if the active session has persisted
    /// history) load its transcript. Call this when the view appears.
    func bootstrap() async {
        await refreshSessions()
        if !ZeroClawSettings.activeSessionId.isEmpty {
            await loadActiveSessionHistory()
        }
    }

    func refreshSessions() async {
        let client = ZeroClawClient()
        let agentAlias = ZeroClawSettings.agentAlias
        do {
            let list = try await client.listSessions()
            // Filter to our agent, sort newest first, cap to session limit.
            let filtered = list.filter { ($0.agentAlias ?? agentAlias) == agentAlias }
            let sorted = filtered.sorted { ($0.lastActivity ?? "") > ($1.lastActivity ?? "") }
            let capped = Array(sorted.prefix(ZeroClawSettings.sessionListLimit))
            var rows = capped.map { session -> SessionRow in
                let short = String(session.sessionId.prefix(8))
                let title = session.name?.isEmpty == false
                    ? session.name!
                    : "session \(short)"
                let count = session.messageCount ?? 0
                let subtitle = "\(count) msg · \(session.lastActivity?.prefix(19) ?? "")"
                return SessionRow(
                    id: session.sessionId,
                    title: title,
                    subtitle: subtitle,
                    messageCount: count,
                    isActive: session.sessionId == activeSessionId
                )
            }
            // Inject the active session as a "(new)" row if the server
            // hasn't seen it yet — happens between mint and first send.
            if !rows.contains(where: { $0.id == activeSessionId }) {
                let short = String(activeSessionId.prefix(8))
                rows.insert(
                    SessionRow(
                        id: activeSessionId,
                        title: "session \(short) (new)",
                        subtitle: "no messages yet",
                        messageCount: 0,
                        isActive: true
                    ),
                    at: 0
                )
            }
            self.sessions = rows
            self.lastError = nil
        } catch {
            JarvisIOSLog.recordIronclawError("zeroclaw list_sessions: \(error.localizedDescription)")
            self.lastError = "list sessions failed: \(error.localizedDescription)"
        }
    }

    func switchSession(to sessionId: String) async {
        guard sessionId != activeSessionId else { return }
        activeSessionId = sessionId
        ZeroClawSettings.activeSessionId = sessionId
        history.removeAll()
        lines.removeAll()
        await loadActiveSessionHistory()
        await refreshSessions()
    }

    func newSession() async {
        activeSessionId = UUID().uuidString
        // Don't persist yet — first send will write it back.
        ZeroClawSettings.activeSessionId = ""
        history.removeAll()
        lines.removeAll()
        statusLine = "new session \(String(activeSessionId.prefix(8)))"
        await refreshSessions()
    }

    private func loadActiveSessionHistory() async {
        let client = ZeroClawClient()
        do {
            let msgs = try await client.sessionMessages(sessionId: activeSessionId)
            self.lines = msgs.map { m -> ChatLine in
                switch m.role {
                case "assistant", "ai":
                    return assistantLine(fromRaw: m.content)
                case "system":
                    return ChatLine(role: .system, text: m.content)
                default:
                    return ChatLine(role: .user, text: m.content)
                }
            }
            // Seed the rolling history from the persisted transcript so the
            // next `/webhook` fallback prepend has prior context.
            history = msgs.suffix(ZeroClawSettings.historyWindow).map { m in
                let role: ChatLine.Role = (m.role == "assistant" || m.role == "ai") ? .assistant : .user
                return (role, m.content)
            }
        } catch {
            JarvisIOSLog.recordIronclawError(
                "zeroclaw session_messages(\(activeSessionId)): \(error.localizedDescription)"
            )
            // Don't surface — empty transcript is fine for a fresh session.
        }
    }

    // ---- Send ---------------------------------------------------------------

    func send() async {
        let text = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !inFlight else { return }

        inFlight = true
        defer { inFlight = false }

        // Local echo first so the user sees their bubble even if the call
        // is slow / fails.
        lines.append(ChatLine(role: .user, text: text))
        pushHistory(.user, text)
        input = ""
        statusLine = ZeroClawSettings.preferStreaming ? "ws: sending…" : "webhook: sending…"
        lastError = nil

        let client = ZeroClawClient()
        let composed = composeWireText(user: text)
        let sessionId = activeSessionId

        do {
            let reply: String
            if ZeroClawSettings.preferStreaming {
                reply = try await client.sendViaWS(message: composed, sessionId: sessionId)
            } else {
                let resp = try await client.webhook(message: composed)
                reply = resp.response ?? (resp.status == "duplicate"
                    ? "(duplicate request — gateway returned previous reply)"
                    : "")
            }
            if reply.isEmpty {
                statusLine = "(empty reply)"
            } else {
                lines.append(assistantLine(fromRaw: reply))
                pushHistory(.assistant, reply)
                statusLine = "reply received (\(reply.count) chars)"
            }
            // First successful send → persist session id so a relaunch
            // resumes here. Matches desktop semantics.
            if ZeroClawSettings.activeSessionId.isEmpty {
                ZeroClawSettings.activeSessionId = sessionId
            }
            await refreshSessions()
        } catch {
            statusLine = "error"
            lastError = error.localizedDescription
            lines.append(ChatLine(role: .system, text: "[error] \(error.localizedDescription)"))
            JarvisIOSLog.recordIronclawError("zeroclaw send: \(error.localizedDescription)")
        }
    }

    // ---- Helpers ------------------------------------------------------------

    private func pushHistory(_ role: ChatLine.Role, _ text: String) {
        let cap = ZeroClawSettings.historyWindow
        guard cap > 0 else { return }
        history.append((role, text))
        if history.count > cap {
            history.removeFirst(history.count - cap)
        }
    }

    private func composeWireText(user: String) -> String {
        guard !history.isEmpty else { return user }
        // ZeroClaw 0.8 persists sessions server-side, so prepending history
        // is mostly belt-and-braces. Cheap insurance for cases where the
        // server transcript was truncated/expired.
        var out = "[Conversation so far:\n"
        // Skip the most recent push (which is the current user message we
        // just appended in `send`).
        for (role, text) in history.dropLast() {
            switch role {
            case .user: out += "User: \(text)\n"
            case .assistant: out += "Assistant: \(text)\n"
            case .system: out += "System: \(text)\n"
            }
        }
        out += "]\n\n"
        out += user
        return out
    }

    private func assistantLine(fromRaw raw: String) -> ChatLine {
        let labels = IosChatFormatting.emotionLabels(from: raw)
        let keys = HubProfileSync.mappedEmotionKeysLowercased()
        let unmapped = labels.contains { !keys.contains($0) }
        if !labels.isEmpty {
            HubProfileSync.ensurePlaceholderEmotions(for: labels)
        }
        let display = IosChatFormatting.stripActDelay(raw)
        return ChatLine(
            role: .assistant,
            text: display,
            emotionCaption: labels.last,
            emotionUnmapped: unmapped
        )
    }
}

// MARK: - View

struct ZeroClawChatView: View {
    @Bindable var model: ZeroClawChatViewModel
    @State private var sidebarOpen: Bool = false

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if sidebarOpen {
                sessionsList
                    .frame(maxHeight: 200)
                Divider()
            }
            transcript
            Divider()
            compose
        }
        .task { await model.bootstrap() }
    }

    private var header: some View {
        HStack(spacing: 12) {
            Button {
                sidebarOpen.toggle()
            } label: {
                Image(systemName: sidebarOpen ? "chevron.up" : "list.bullet")
            }
            .accessibilityLabel("Toggle session list")

            Text(headerTitle)
                .font(.headline)
                .lineLimit(1)

            Spacer()

            Button {
                Task { await model.newSession() }
            } label: {
                Image(systemName: "square.and.pencil")
            }
            .accessibilityLabel("New chat")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    private var headerTitle: String {
        let short = String(model.activeSessionId.prefix(8))
        if let err = model.lastError, !err.isEmpty {
            return "session \(short) · err"
        }
        return "session \(short) · \(model.statusLine)"
    }

    private var sessionsList: some View {
        List(model.sessions) { row in
            Button {
                Task { await model.switchSession(to: row.id) }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: row.isActive ? "circle.fill" : "circle")
                        .foregroundStyle(row.isActive ? Color.accentColor : Color.secondary)
                        .font(.caption)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(row.title)
                            .font(.subheadline)
                            .lineLimit(1)
                        Text(row.subtitle)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Spacer()
                    if row.messageCount > 0 {
                        Text("\(row.messageCount)")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .buttonStyle(.plain)
        }
        .listStyle(.plain)
    }

    private var transcript: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(model.lines) { line in
                        bubble(line)
                            .id(line.id)
                    }
                }
                .padding(12)
            }
            .onChange(of: model.lines.count) { _, _ in
                if let last = model.lines.last {
                    withAnimation(.easeOut(duration: 0.15)) {
                        proxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func bubble(_ line: ZeroClawChatViewModel.ChatLine) -> some View {
        HStack {
            if line.role == .user { Spacer(minLength: 40) }
            VStack(alignment: line.role == .user ? .trailing : .leading, spacing: 4) {
                Text(line.text)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(bubbleBg(for: line.role), in: RoundedRectangle(cornerRadius: 12))
                    .foregroundStyle(line.role == .system ? Color.orange : Color.primary)
                    .textSelection(.enabled)
                if let caption = line.emotionCaption, !caption.isEmpty {
                    Text(line.emotionUnmapped ? "emotion: \(caption) (unmapped)" : "emotion: \(caption)")
                        .font(.caption2)
                        .foregroundStyle(line.emotionUnmapped ? .orange : .secondary)
                }
            }
            if line.role != .user { Spacer(minLength: 40) }
        }
    }

    private func bubbleBg(for role: ZeroClawChatViewModel.ChatLine.Role) -> Color {
        switch role {
        case .user: return Color.accentColor.opacity(0.18)
        case .assistant: return Color.gray.opacity(0.18)
        case .system: return Color.orange.opacity(0.15)
        }
    }

    private var compose: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField("Message ZeroClaw…", text: $model.input, axis: .vertical)
                .lineLimit(1...5)
                .textFieldStyle(.roundedBorder)
                .submitLabel(.send)
                .onSubmit {
                    Task { await model.send() }
                }
            Button {
                Task { await model.send() }
            } label: {
                Image(systemName: model.inFlight ? "ellipsis" : "paperplane.fill")
                    .padding(8)
            }
            .disabled(model.input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.inFlight)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
    }
}
