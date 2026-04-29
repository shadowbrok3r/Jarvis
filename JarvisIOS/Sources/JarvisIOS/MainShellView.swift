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
    @AppStorage("jarvis.avatarChatOverlay") private var showAvatarChatOverlay = false
    @AppStorage(HubProfileSync.userDefaultsBaseURLKey) private var hubBaseURL: String = ""
    @AppStorage(HubProfileSync.userDefaultsSecondaryBaseURLKey) private var hubSecondaryBaseURL: String = ""
    @AppStorage(HubProfileSync.userDefaultsAuthTokenKey) private var hubAuthToken: String = ""
    @AppStorage(HubProfileSync.Gateway.userDefaultsBaseURLKey) private var gatewayBaseURL: String = ""
    @AppStorage(HubProfileSync.Gateway.userDefaultsSecondaryBaseURLKey) private var gatewaySecondaryBaseURL: String = ""
    @AppStorage(HubProfileSync.Gateway.userDefaultsAuthTokenKey) private var gatewayAuthToken: String = ""
    @State private var syncStatus: String = ""
    @State private var syncInFlight = false
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
                                    Button {
                                        showAvatarChatOverlay.toggle()
                                    } label: {
                                        Image(systemName: showAvatarChatOverlay ? "bubble.left.and.text.bubble.fill" : "bubble.left.and.text.bubble")
                                            .font(.title3)
                                            .padding(10)
                                            .background(.ultraThinMaterial, in: Circle())
                                    }
                                    .padding(.top, 6)
                                    .padding(.trailing, 8)
                                    .accessibilityLabel(showAvatarChatOverlay ? "Hide chat overlay" : "Show chat overlay")
                                }
                            }
                            .overlay(alignment: .bottom) {
                                if shellTab == .avatar, showAvatarChatOverlay {
                                    GatewayChatView(
                                        model: gatewayChatModel,
                                        compact: true,
                                        onDismissCompact: { showAvatarChatOverlay = false }
                                    )
                                    .frame(height: min(380, h * 0.46))
                                    .frame(maxWidth: .infinity)
                                    .background(.ultraThinMaterial)
                                    .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 16, style: .continuous)
                                            .strokeBorder(Color.primary.opacity(0.12), lineWidth: 1)
                                    )
                                    .padding(.horizontal, 10)
                                    .padding(.bottom, 8)
                                    .shadow(color: .black.opacity(0.2), radius: 12, y: 4)
                                }
                            }

                        GatewayChatView(model: gatewayChatModel)
                            .frame(width: w, height: h)
                            .background(Color(uiColor: .systemGroupedBackground))
                            .opacity(shellTab == .chat ? 1 : 0)
                            .allowsHitTesting(shellTab == .chat)
                            .zIndex(shellTab == .chat ? 1 : 0)

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
        }
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
                    Button {
                        Task { await runHubSync() }
                    } label: {
                        Text("Sync profile")
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
        let ok = await HubProfileSync.syncFromHubToCache(progress: progress)
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
