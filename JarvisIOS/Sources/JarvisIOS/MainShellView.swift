import SwiftUI
import UIKit

/// Root shell: avatar (Bevy), Chat, About, and Logs.
///
/// `JarvisBevyView` stays mounted for the app lifetime (opacity + zIndex switch tabs). If we used
/// `switch` and removed the Bevy branch, SwiftUI would call `dismantleUIView` → `teardown()` →
/// cancel the async bootstrap `Task` before `startRenderer` (same‑ms “Task cancelled” in logs).
///
/// **Do not** put `.id(bevySessionId)` on `JarvisBevyView`: that destroys the `UIView` on every hub
/// reload, so one coordinator logs real bounds while a brand‑new sibling view is still 0×0 and the
/// bootstrap `Task` is cancelled. Pass `sessionKey:` instead so the same view resets in `updateUIView`.
struct MainShellView: View {
    private enum ShellTab: Int, CaseIterable, Identifiable {
        case avatar, chat, about, logs
        var id: Int { rawValue }
    }

    @State private var shellTab: ShellTab = .avatar
    @State private var bevySessionId = 0
    @State private var gatewayChatModel = GatewayChatViewModel()
    @State private var zeroClawChatModel = ZeroClawChatViewModel()
    /// Chat backend chosen by the user (mirrors desktop `gateway.backend`).
    /// `.ironclaw` keeps the historical `GatewayChatView` path; `.zeroclaw`
    /// activates the new `ZeroClawChatView`. Persisted in `UserDefaults`
    /// under `jarvis.chat.backend`.
    @AppStorage(ZeroClawSettings.userDefaultsBackendKey) private var chatBackendRaw: String = ChatBackend.ironclaw.rawValue
    @AppStorage("jarvis.avatarBottomPanel") private var showAvatarBottomPanel = false
    @AppStorage("jarvis.avatarBottomPanelHeight") private var avatarBottomPanelHeight: Double = 380
    @State private var liveBottomPanelHeight: CGFloat = 380
    @AppStorage("jarvis.avatarBottomPanelTab") private var avatarBottomPanelTabRaw: String = AvatarBottomPanelTab.chat.rawValue
    @AppStorage(HubProfileSync.userDefaultsBaseURLKey) private var hubBaseURL: String = ""
    @AppStorage(HubProfileSync.userDefaultsSecondaryBaseURLKey) private var hubSecondaryBaseURL: String = ""
    @AppStorage(HubProfileSync.userDefaultsAuthTokenKey) private var hubAuthToken: String = ""
    @AppStorage(HubProfileSync.Gateway.userDefaultsBaseURLKey) private var gatewayBaseURL: String = ""
    @AppStorage(HubProfileSync.Gateway.userDefaultsSecondaryBaseURLKey) private var gatewaySecondaryBaseURL: String = ""
    @AppStorage(HubProfileSync.Gateway.userDefaultsAuthTokenKey) private var gatewayAuthToken: String = ""
    @AppStorage(HubProfileSync.Kokoro.userDefaultsBaseURLKey) private var kokoroBaseURL: String = ""
    @AppStorage(HubProfileSync.Kokoro.userDefaultsVoiceKey) private var kokoroVoice: String = ""
    @State private var syncStatus: String = ""
    @State private var syncInFlight = false
    @AppStorage("jarvis.hub.syncScope") private var syncScope: String = "all"
    @State private var hubSyncProgress: Progress?
    @State private var discoveredVrms: [String] = []
    @State private var manifestModelHint: String = ""
    @AppStorage(HubProfileSync.IosAvatarCustomize.userDefaultsModelRelPathOverrideKey) private var modelOverrideRel: String = ""
    @AppStorage(HubProfileSync.IosAvatarCustomize.userDefaultsIdleVrmaRelPathOverrideKey) private var idleOverrideRel: String = ""
    @AppStorage(HubProfileSync.IosSceneCustomize.userDefaultsGroundOverrideKey) private var sceneGroundOverride: String = ""
    @AppStorage(HubProfileSync.IosSceneCustomize.userDefaultsBackgroundLinearRgbaKey) private var sceneBackgroundLinearRgba: String = ""

    var body: some View {
        // `safeAreaInset` did not shrink this stack reliably across NavigationStack children; a plain
        // `VStack` reserves the tab chrome height so Chat / About / Logs stay above the bar and
        // Bevy’s `GeometryReader` height matches the Metal layer (no strip between viewer and tabs).
        VStack(spacing: 0) {
            ZStack {
                GeometryReader { geo in
                    let w = max(1, geo.size.width)
                    let h = max(1, geo.size.height)
                    ZStack {
                        JarvisBevyView(sessionKey: bevySessionId, avatarTabVisible: shellTab == .avatar)
                            .frame(width: w, height: h)
                            // Keep Metal + egui at full layout height when the chat composer keyboard is visible (overlay or system keyboard).
                            .ignoresSafeArea(.keyboard, edges: .bottom)
                            // Respect top safe area so Metal + egui sit below the status bar (tappable UI, no black strip under system chrome).
                            .opacity(shellTab == .avatar ? 1 : 0)
                            .allowsHitTesting(shellTab == .avatar)
                            .zIndex(shellTab == .avatar ? 1 : 0)
                            .overlay(alignment: .topTrailing) {
                                if shellTab == .avatar {
                                    HStack(spacing: 6) {
                                        ForEach(AvatarBottomPanelTab.allCases) { tab in
                                            Button {
                                                openAvatarBottomPanel(tab: tab)
                                            } label: {
                                                Image(systemName: tab.systemImage)
                                                    .font(.body)
                                                    .padding(8)
                                                    .background(
                                                        showAvatarBottomPanel && avatarBottomPanelTab == tab
                                                            ? Color.accentColor.opacity(0.25)
                                                            : Color.clear,
                                                        in: Circle()
                                                    )
                                            }
                                            .accessibilityLabel(tab.title)
                                        }
                                    }
                                    .padding(8)
                                    .background(.ultraThinMaterial, in: Capsule())
                                    .padding(.top, 6)
                                    .padding(.trailing, 8)
                                }
                            }
                            .overlay(alignment: .bottom) {
                                if shellTab == .avatar, showAvatarBottomPanel {
                                    AvatarToolsOverlay(
                                        selectedTab: Binding(
                                            get: { avatarBottomPanelTab },
                                            set: { avatarBottomPanelTabRaw = $0.rawValue }
                                        ),
                                        panelHeight: Binding(
                                            get: { liveBottomPanelHeight },
                                            set: { newHeight in
                                                liveBottomPanelHeight = newHeight
                                                avatarBottomPanelHeight = Double(newHeight)
                                            }
                                        ),
                                        viewportHeight: h,
                                        onDismiss: { showAvatarBottomPanel = false },
                                        chatModel: gatewayChatModel
                                    )
                                }
                            }

                        // Chat tab: route to the active backend's view. Both
                        // views are kept mounted (opacity-switched) so model
                        // state survives a quick backend flip without a
                        // remount-induced reset.
                        let activeBackend = ChatBackend(rawValue: chatBackendRaw) ?? .ironclaw
                        GatewayChatView(model: gatewayChatModel)
                            .frame(width: w, height: h)
                            .background(Color(uiColor: .systemGroupedBackground))
                            .opacity(shellTab == .chat && activeBackend == .ironclaw ? 1 : 0)
                            .allowsHitTesting(shellTab == .chat && activeBackend == .ironclaw)
                            .zIndex(shellTab == .chat && activeBackend == .ironclaw ? 1 : 0)

                        ZeroClawChatView(model: zeroClawChatModel)
                            .frame(width: w, height: h)
                            .background(Color(uiColor: .systemGroupedBackground))
                            .opacity(shellTab == .chat && activeBackend == .zeroclaw ? 1 : 0)
                            .allowsHitTesting(shellTab == .chat && activeBackend == .zeroclaw)
                            .zIndex(shellTab == .chat && activeBackend == .zeroclaw ? 1 : 0)

                        aboutStack
                            .frame(width: w, height: h)
                            .background(Color(uiColor: .systemGroupedBackground))
                            .opacity(shellTab == .about ? 1 : 0)
                            .allowsHitTesting(shellTab == .about)
                            .zIndex(shellTab == .about ? 1 : 0)

                        DebugLogsView()
                            .frame(width: w, height: h)
                            .opacity(shellTab == .logs ? 1 : 0)
                            .allowsHitTesting(shellTab == .logs)
                            .zIndex(shellTab == .logs ? 1 : 0)
                    }
                    .frame(width: w, height: h)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }

            VStack(spacing: 0) {
                Rectangle()
                    .fill(Color.primary.opacity(0.1))
                    .frame(height: 1 / max(UIScreen.main.scale, 1))
                HStack(spacing: 0) {
                    shellTabButton(.avatar, title: "Avatar", systemImage: "person.crop.circle")
                    shellTabButton(.chat, title: "Chat", systemImage: "bubble.left.and.bubble.right")
                    shellTabButton(.about, title: "About", systemImage: "info.circle")
                    shellTabButton(.logs, title: "Logs", systemImage: "ladybug.fill")
                }
                .padding(.top, 6)
                .padding(.bottom, 6)
                .safeAreaPadding(.bottom, 2)
            }
            .frame(maxWidth: .infinity)
            .background(.bar)
        }
        .onAppear {
            gatewayChatModel.onAppear()
            migrateLegacyAvatarChatOverlayFlag()
            liveBottomPanelHeight = CGFloat(avatarBottomPanelHeight)
        }
    }

    private var avatarBottomPanelTab: AvatarBottomPanelTab {
        AvatarBottomPanelTab(rawValue: avatarBottomPanelTabRaw) ?? .chat
    }

    private func openAvatarBottomPanel(tab: AvatarBottomPanelTab) {
        avatarBottomPanelTabRaw = tab.rawValue
        if avatarBottomPanelHeight < 160 {
            avatarBottomPanelHeight = 380
        }
        liveBottomPanelHeight = CGFloat(avatarBottomPanelHeight)
        showAvatarBottomPanel = true
    }

    /// One-time migration from the old chat-only overlay toggle.
    private func migrateLegacyAvatarChatOverlayFlag() {
        let key = "jarvis.avatarChatOverlay"
        guard UserDefaults.standard.object(forKey: key) != nil else { return }
        if UserDefaults.standard.bool(forKey: key) {
            showAvatarBottomPanel = true
            avatarBottomPanelTabRaw = AvatarBottomPanelTab.chat.rawValue
        }
        UserDefaults.standard.removeObject(forKey: key)
    }

    private var aboutStack: some View {
        NavigationStack {
            List {
                Section("Build") {
                    Text(jarvis_ios_version().toString())
                        .font(.footnote)
                        .textSelection(.enabled)
                }
                Section("ACT → avatar (offline)") {
                    NavigationLink("Emotion & animation map") {
                        ActEmotionMapEditorView()
                    }
                    Text(
                        "Edits the same `config/emotions.json` layout as desktop: ACT labels → VRM expression + pose-library animation JSON. " +
                            "Animation filenames are listed from your synced hub asset root."
                    )
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    NavigationLink("Play saved motion (JSON)") {
                        SavedAnimationsPlayView()
                    }
                }
                Section("Chat backend") {
                    Picker("Active backend", selection: $chatBackendRaw) {
                        ForEach(ChatBackend.allCases) { backend in
                            Text(backend.displayLabel).tag(backend.rawValue)
                        }
                    }
                    .pickerStyle(.segmented)
                    Text(
                        "IronClaw uses the URLs below (HTTP + SSE). ZeroClaw uses the section further down. Switching takes effect immediately — the chat tab re-renders with the new backend."
                    )
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                }
                Section("IronClaw gateway (chat)") {
                    TextField("Gateway base URL (http://host:3000)", text: $gatewayBaseURL)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)
                        .autocorrectionDisabled()
                    TextField("Fallback gateway URL (optional)", text: $gatewaySecondaryBaseURL)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)
                        .autocorrectionDisabled()
                    SecureField("Gateway bearer token (optional)", text: $gatewayAuthToken)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Text(
                        "Chat uses HTTP + SSE (tries primary URL, then fallback). The channel hub WebSocket uses the hub URLs and hub token below."
                    )
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                }
                Section("ZeroClaw gateway (chat)") {
                    TextField(
                        "ZeroClaw base URL (https://claw.example.com)",
                        text: Binding(
                            get: { ZeroClawSettings.baseURL },
                            set: { ZeroClawSettings.baseURL = $0 }
                        )
                    )
                    .textInputAutocapitalization(.never)
                    .keyboardType(.URL)
                    .autocorrectionDisabled()
                    SecureField(
                        "Bearer token (from `POST /pair`)",
                        text: Binding(
                            get: { ZeroClawSettings.authToken },
                            set: { ZeroClawSettings.authToken = $0 }
                        )
                    )
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    TextField(
                        "Agent alias (matches `[agents.<alias>]` on the gateway)",
                        text: Binding(
                            get: { ZeroClawSettings.agentAlias },
                            set: { ZeroClawSettings.agentAlias = $0 }
                        )
                    )
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    Toggle(
                        "Prefer streaming (WS) over webhook",
                        isOn: Binding(
                            get: { ZeroClawSettings.preferStreaming },
                            set: { ZeroClawSettings.preferStreaming = $0 }
                        )
                    )
                    Text(
                        "WS path: `/ws/chat?agent=<alias>&session_id=…&token=…`. ZeroClaw returns the whole reply in one `done` frame — there's no per-token streaming on the iOS chat surface today. Sessions persist across launches under `\(ZeroClawSettings.userDefaultsActiveSessionIdKey)`."
                    )
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                }
                Section("Kokoro TTS (chat voice)") {
                    TextField("Kokoro base URL (http://host:8880)", text: $kokoroBaseURL)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)
                        .autocorrectionDisabled()
                    TextField("Voice (e.g. af_heart or af_aoede(1.0)+af_nicole(1.0))", text: $kokoroVoice)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Text(
                        "When set, each final assistant **response** on the Chat tab fetches `/v1/audio/speech` with `stream: false` and plays WAV — same contract as desktop. A2F is not on-device yet; use the desktop MCP `a2f_from_text` or wire gRPC separately."
                    )
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                }
                Section("Hub profile") {
                    TextField("Base URL (http://host:6121)", text: $hubBaseURL)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)
                        .autocorrectionDisabled()
                    TextField("Fallback hub URL (optional)", text: $hubSecondaryBaseURL)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)
                        .autocorrectionDisabled()
                    SecureField("Bearer token (optional)", text: $hubAuthToken)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Picker("Sync scope", selection: $syncScope) {
                        Text("All").tag("all")
                        Text("Models").tag("models")
                        Text("Animations").tag("animations")
                        Text("Poses").tag("poses")
                    }
                    .pickerStyle(.segmented)
                    Text("All = full profile (unchanged files are reused). A single scope re-downloads only that kind — everything else is reused from the last sync.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Button {
                        Task { await runHubSync() }
                    } label: {
                        Text(syncScope == "all" ? "Sync profile" : "Sync \(syncScope)")
                    }
                    .disabled(syncInFlight || hubBaseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    if syncInFlight, let prog = hubSyncProgress {
                        ProgressView(prog)
                            .padding(.vertical, 4)
                        TimelineView(.periodic(from: .now, by: 0.12)) { _ in
                            Text(prog.localizedDescription)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    if !syncStatus.isEmpty {
                        Text(syncStatus)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Section("Scene (this device)") {
                    Picker("Ground plane", selection: $sceneGroundOverride) {
                        Text("Hub manifest").tag("")
                        Text("Force show").tag("show")
                        Text("Force hide").tag("hide")
                    }
                    TextField("Background linear r,g,b,a (optional)", text: $sceneBackgroundLinearRgba)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Text(
                        "Ground: empty = use hub `graphics.show_ground_plane`. Background: empty = use hub `avatar.background_color`. Values are **linear** RGBA (e.g. `0.05,0.05,0.08,1`). Apply with the button in Avatar model."
                    )
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                }
                Section("Avatar model (local)") {
                    Picker("VRM file", selection: $modelOverrideRel) {
                        Text("Manifest default — \(manifestModelHint)")
                            .lineLimit(2)
                            .tag("")
                        ForEach(discoveredVrms, id: \.self) { rel in
                            Text(rel).tag(rel)
                        }
                    }
                    TextField("Idle VRMA override (optional, relative path)", text: $idleOverrideRel)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("Rescan .vrm under asset root") {
                        refreshAvatarModelDiscovery()
                    }
                    Button("Apply overrides & reload avatar") {
                        HubProfileSync.applyIosAvatarOverrideEnvFromUserDefaults()
                        JarvisIOSLog.recordUI("avatar overrides applied → reloadProfile")
                        JarvisBevySession.reloadProfileFromDiskManifest()
                    }
                    Text(
                        "Scans every `.vrm` under the active hub cache (or bundled `JARVIS_ASSET_ROOT`). Pick one to override the hub manifest on this device only. Clear the picker to use the manifest again."
                    )
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                }
                Section("Debug") {
                    Text(
                        "The line “reused persisted hub cache” is emitted when prepareForBevyBootstrap runs (Avatar screen with non‑zero layout, or the button below). Sync alone writes files and env but does not run that bootstrap path."
                    )
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    Button("Log hub cache + disk + env") {
                        HubProfileSync.logHubCacheDiagnostics()
                    }
                    Button("Run prepareForBevyBootstrap (see Hub logs)") {
                        Task {
                            await HubProfileSync.prepareForBevyBootstrap()
                            JarvisIOSLog.recordUI("manual prepareForBevyBootstrap finished (check Hub lines above)")
                        }
                    }
                    Button("Reload Bevy view (bump session)") {
                        bevySessionId += 1
                        JarvisIOSLog.recordUI("manual bevySessionId → \(bevySessionId)")
                    }
                    Button("Clear persisted hub cache keys", role: .destructive) {
                        HubProfileSync.clearPersistedHubCachePointers()
                        JarvisIOSLog.recordUI("cleared UserDefaults hub cache pointers (next bootstrap may re-download)")
                    }
                }
            }
            .navigationTitle("About")
            .onAppear {
                HubProfileSync.migrateAuthTokenFromUserDefaultsIfNeeded()
                HubProfileSync.migrateGatewayAuthTokenFromUserDefaultsIfNeeded()
                HubProfileSync.persistAuthTokenFromUI(hubAuthToken)
                HubProfileSync.persistGatewayAuthTokenFromUI(gatewayAuthToken)
                refreshAvatarModelDiscovery()
            }
            .onChange(of: hubBaseURL) { _, _ in
                IronclawConnectivity.shared.start()
            }
            .onChange(of: hubSecondaryBaseURL) { _, _ in
                IronclawConnectivity.shared.start()
            }
            .onChange(of: hubAuthToken) { _, newValue in
                HubProfileSync.persistAuthTokenFromUI(newValue)
                IronclawConnectivity.shared.start()
            }
            .onChange(of: gatewayAuthToken) { _, newValue in
                HubProfileSync.persistGatewayAuthTokenFromUI(newValue)
            }
        }
    }

    private func shellTabButton(_ tab: ShellTab, title: String, systemImage: String) -> some View {
        Button {
            shellTab = tab
        } label: {
            VStack(spacing: 3) {
                Image(systemName: systemImage)
                    .imageScale(.medium)
                Text(title)
                    .font(.caption2)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 2)
            .foregroundStyle(shellTab == tab ? Color.accentColor : Color.secondary)
        }
        .buttonStyle(.plain)
    }

    @MainActor
    private func runHubSync() async {
        syncInFlight = true
        syncStatus = "Syncing…"
        let progress = Progress(totalUnitCount: 1)
        progress.completedUnitCount = 0
        hubSyncProgress = progress
        HubProfileSync.persistAuthTokenFromUI(hubAuthToken)
        let categories: Set<HubProfileSync.SyncCategory>? =
            HubProfileSync.SyncCategory(rawValue: syncScope).map { [$0] }
        let ok = await HubProfileSync.syncFromHubToCache(progress: progress, categories: categories)
        hubSyncProgress = nil
        syncInFlight = false
        if ok {
            JarvisIOSLog.recordUI(
                "runHubSync: success → hot-reload Bevy profile (or bump session if no renderer yet)."
            )
        } else {
            JarvisIOSLog.recordUIError("runHubSync: sync failed (see HubProfile logs)")
        }
        syncStatus = ok ? "Saved. Reloading avatar…" : "Sync failed — check URL, token, and network."
        if ok {
            JarvisBevySession.reloadProfileFromDiskManifest()
            IronclawConnectivity.shared.start()
            refreshAvatarModelDiscovery()
        }
    }

    private func refreshAvatarModelDiscovery() {
        discoveredVrms = HubProfileSync.listDiscoveredVrmRelativePaths()
        if let m = HubProfileSync.readHubManifestModelPath(), !m.isEmpty {
            manifestModelHint = m
        } else {
            manifestModelHint = "(no manifest on disk — sync hub or use bundled profile)"
        }
    }
}
