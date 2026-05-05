import BridgeFFI
import SwiftUI

/// In-app console for Swift + Rust `jarvis_ios_line!` / `jarvis_ios_debug_log_snapshot` (no Xcode Console required).
struct DebugLogsView: View {
    @State private var bus = JarvisIOSLogBus.shared
    @State private var poll = false
    @State private var micMonitor = JarvisMicLevelMonitor()
    @State private var cameraSession = JarvisCameraSession()
    @State private var uploadStatus: String? = nil
    @State private var uploading = false

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 8) {
                micDebugSection
                cameraDebugSection
                if let status = uploadStatus {
                    Text(status)
                        .font(.caption.monospaced())
                        .foregroundStyle(.green)
                        .padding(.horizontal, 8)
                        .lineLimit(2)
                }
                ScrollView {
                    Text(bus.displayText)
                        .font(.system(.footnote, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                }
            }
            .background(Color.black.opacity(0.92))
            .navigationTitle("Logs")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        pushLogsToHub()
                    } label: {
                        if uploading {
                            ProgressView().controlSize(.small)
                        } else {
                            Label("Push to Hub", systemImage: "arrow.up.circle")
                        }
                    }
                    .disabled(uploading)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Clear") {
                        bus.clearAll()

                    }
                }
            }
            .onAppear {
                poll = true
                refreshRustSnapshot()
            }
            .onDisappear { poll = false }
            .onReceive(Timer.publish(every: 0.25, on: .main, in: .common).autoconnect()) { _ in
                guard poll else { return }
                refreshRustSnapshot()
            }
        }
    }

    private var micDebugSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Mic / AVAudioSession (debug)")
                .font(.caption)
                .fontWeight(.semibold)
                .foregroundStyle(.secondary)
            HStack(spacing: 8) {
                Button("Prepare AV session") {
                    micMonitor.prepareSessionOnly()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button("Start mic meter") {
                    micMonitor.startMeter()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(micMonitor.isRunning)
                Button("Stop") {
                    micMonitor.stopMeter()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(!micMonitor.isRunning)
            }
            HStack(spacing: 10) {
                ProgressView(value: micMonitor.normalizedLevel, total: 1)
                    .tint(.green)
                Text(String(format: "%.0f%%", micMonitor.normalizedLevel * 100))
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .frame(width: 40, alignment: .trailing)
            }
            if let err = micMonitor.lastError, !err.isEmpty {
                Text(err)
                    .font(.caption2)
                    .foregroundStyle(.red)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.08))
    }

    private var cameraDebugSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Camera (debug)")
                .font(.caption)
                .fontWeight(.semibold)
                .foregroundStyle(.secondary)
            Text(cameraPermissionLine)
                .font(.caption2)
                .foregroundStyle(.secondary)
            HStack(spacing: 8) {
                Button("Request access") {
                    cameraSession.requestVideoAccessIfNeeded()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button("Start preview") {
                    cameraSession.startPreview()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(!cameraSession.canStartPreview || cameraSession.isRunning)
                Button("Stop") {
                    cameraSession.stopPreview()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(!cameraSession.isRunning)
            }
            if !cameraSession.activeCameraSummary.isEmpty {
                Text(cameraSession.activeCameraSummary)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
            }
            JarvisCameraPreviewRepresentable(session: cameraSession.captureSession)
                .frame(maxWidth: .infinity)
                .aspectRatio(4 / 3, contentMode: .fit)
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(Color.primary.opacity(0.12), lineWidth: 1)
                )
            if let err = cameraSession.lastError, !err.isEmpty {
                Text(err)
                    .font(.caption2)
                    .foregroundStyle(.red)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.08))
        .onAppear {
            cameraSession.refreshAuthorizationFromSystem()
        }
    }

    private var cameraPermissionLine: String {
        switch cameraSession.authorization {
        case .notDetermined: "Permission: not determined — tap Request access if needed."
        case .authorized: "Permission: authorized."
        case .denied: "Permission: denied — enable Camera in Settings to preview."
        case .restricted: "Permission: restricted (e.g. parental controls)."
        }
    }

    private func refreshRustSnapshot() {
        let s = jarvis_ios_debug_log_snapshot().toString()
        bus.setRustSnapshot(s)
    }

    private func pushLogsToHub() {
        let hubURL = UserDefaults.standard.string(forKey: HubProfileSync.userDefaultsBaseURLKey) ?? ""
        guard !hubURL.isEmpty else {
            uploadStatus = "No hub URL configured"
            return
        }
        uploading = true
        uploadStatus = nil
        Task {
            let ok = await JarvisIOSCrashLog.uploadCurrentSnapshot(hubBaseURL: hubURL)
            uploading = false
            uploadStatus = ok ? "Pushed to hub → .dev/ios_live_snapshot.txt" : "Push failed — check logs"
            try? await Task.sleep(nanoseconds: 4_000_000_000)
            uploadStatus = nil
        }
    }
}
