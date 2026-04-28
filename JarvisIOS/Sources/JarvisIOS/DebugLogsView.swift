import BridgeFFI
import SwiftUI

/// In-app console for Swift + Rust `jarvis_ios_line!` / `jarvis_ios_debug_log_snapshot` (no Xcode Console required).
struct DebugLogsView: View {
    @State private var bus = JarvisIOSLogBus.shared
    @State private var poll = false

    var body: some View {
        NavigationStack {
            ScrollView {
                Text(bus.displayText)
                    .font(.system(.footnote, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
            }
            .background(Color.black.opacity(0.92))
            .navigationTitle("Logs")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
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

    private func refreshRustSnapshot() {
        let s = jarvis_ios_debug_log_snapshot().toString()
        bus.setRustSnapshot(s)
    }
}
