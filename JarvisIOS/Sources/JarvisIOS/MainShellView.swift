import SwiftUI
import UIKit

/// Root shell: avatar (Bevy) tab plus a small About screen for the Rust bridge version.
struct MainShellView: View {
    @State private var bevySessionId = 0
    @AppStorage(HubProfileSync.userDefaultsBaseURLKey) private var hubBaseURL: String = ""
    @AppStorage(HubProfileSync.userDefaultsAuthTokenKey) private var hubAuthToken: String = ""
    @State private var syncStatus: String = ""
    @State private var syncInFlight = false
    @State private var hubSyncProgress: Progress?

    var body: some View {
        TabView {
            // TabView often proposes 0×0 to the first tab; GeometryReader + screen fallback yields a real size.
            GeometryReader { geo in
                let screen = UIScreen.main.bounds
                let gw = geo.size.width
                let gh = geo.size.height
                let w = (gw.isFinite && gw > 10) ? gw : screen.width
                let h = (gh.isFinite && gh > 10) ? gh : screen.height
                JarvisBevyView()
                    .id(bevySessionId)
                    .frame(width: w, height: h)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .ignoresSafeArea()
            .tabItem {
                    Label("Avatar", systemImage: "person.crop.circle")
                }

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
                            "The line “reused persisted hub cache” is emitted when prepareForBevyBootstrap runs (Avatar tab once the view has size, or the button below). Sync alone writes files and env but does not run that bootstrap path."
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
                // Do not clear hub cache pointers when the URL field changes. Each keystroke used to fire
                // onChange and wipe UserDefaults, breaking prepareForBevyBootstrap after a successful sync.
                // HubProfileSync.applyPersistedHubCacheEnvIfValid already refuses another host’s cache.
                .onChange(of: hubAuthToken) { _, newValue in
                    HubProfileSync.persistAuthTokenFromUI(newValue)
                }
            }
            .tabItem {
                Label("About", systemImage: "info.circle")
            }

            DebugLogsView()
                .tabItem {
                    Label("🐛 Logs", systemImage: "ladybug.fill")
                }
        }
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
                "runHubSync: success → bump bevySessionId. Open Avatar (or tap “Run prepare…” in About → Debug) to see prepareForBevyBootstrap / cache reuse lines."
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
