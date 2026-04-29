import SwiftUI

/// Lists pose-library JSON files under the active asset root and queues native playback on the Bevy view.
struct SavedAnimationsPlayView: View {
    @State private var paths: [String] = []
    @State private var status: String = ""

    var body: some View {
        List {
            if !status.isEmpty {
                Section {
                    Text(status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Section {
                Text(
                    "Plays Kimodo / MCP-style JSON clips on device (relative to hub sync asset root). " +
                        "Use **Sync profile** on About to download `animations/*.json` from the desktop hub."
                )
                .font(.caption2)
                .foregroundStyle(.secondary)
            }
            Section("Clips") {
                ForEach(paths, id: \.self) { rel in
                    HStack {
                        Text(rel)
                            .font(.caption)
                            .lineLimit(2)
                        Spacer()
                        Button("Play") {
                            JarvisBevySession.queueAnimJson(path: rel)
                            status = "Queued \(rel)"
                            JarvisIOSLog.recordUI("queueAnimJson \(rel)")
                        }
                        .buttonStyle(.borderedProminent)
                    }
                }
            }
        }
        .navigationTitle("Saved motions")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            paths = HubProfileSync.listDiscoveredAnimationJsonRelativePaths()
            if paths.isEmpty {
                status = "No JSON animations under asset root — sync hub or bundle assets."
            }
        }
    }
}
