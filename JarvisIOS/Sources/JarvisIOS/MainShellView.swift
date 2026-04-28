import SwiftUI

/// Root shell: avatar (Bevy), About, and Logs.
///
/// We avoid `TabView` for the Bevy tab: on device, SwiftUI often leaves `UIViewRepresentable` at 0×0
/// (even inside `GeometryReader`), so `jarvis_renderer_new` never runs. A plain `VStack` + custom
/// tab bar gets a full-window `frame(maxWidth:maxHeight:)` for the Metal host.
struct MainShellView: View {
    private enum ShellTab: Int, CaseIterable, Identifiable {
        case avatar, about, logs
        var id: Int { rawValue }
    }

    @State private var shellTab: ShellTab = .avatar
    @State private var bevySessionId = 0
    @AppStorage(HubProfileSync.userDefaultsBaseURLKey) private var hubBaseURL: String = ""
    @AppStorage(HubProfileSync.userDefaultsAuthTokenKey) private var hubAuthToken: String = ""
    @State private var syncStatus: String = ""
    @State private var syncInFlight = false
    @State private var hubSyncProgress: Progress?

    var body: some View {
        VStack(spacing: 0) {
            Group {
                switch shellTab {
                case .avatar:
                    JarvisBevyView()
                        .id(bevySessionId)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .ignoresSafeArea()
                case .about:
                    aboutStack
                case .logs:
                    DebugLogsView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            Divider()
            HStack(spacing: 0) {
                shellTabButton(.avatar, title: "Avatar", systemImage: "person.crop.circle")
                shellTabButton(.about, title: "About", systemImage: "info.circle")
                shellTabButton(.logs, title: "Logs", systemImage: "ladybug.fill")
            }
            .padding(.top, 6)
            .padding(.bottom, 6)
            .background(.bar)
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
                Section("Hub profile") {
                    TextField("Base URL (http://host:6121)", text: $hubBaseURL)
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
                HubProfileSync.persistAuthTokenFromUI(hubAuthToken)
            }
            .onChange(of: hubAuthToken) { _, newValue in
                HubProfileSync.persistAuthTokenFromUI(newValue)
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
                "runHubSync: success → bump bevySessionId. Choose Avatar (bottom) or Debug → Run prepare… to see prepareForBevyBootstrap / cache reuse lines."
            )
        } else {
            JarvisIOSLog.recordUIError("runHubSync: sync failed (see HubProfile logs)")
        }
        syncStatus = ok ? "Saved. Reloading avatar…" : "Sync failed — check URL, token, and network."
        if ok {
            bevySessionId += 1
        }
    }
}
