import SwiftUI

struct ContentView: View {
    var body: some View {
        MainShellView()
            .onAppear {
                // Prefs + saved log level must be in the process env before Rust installs the log subscriber.
                HubProfileSync.applyIosBootEnvironment()

                // Set up persistent crash log FIRST (before Bevy / hub sync)
                // so that any log line from this point is written to the file.
                JarvisIOSCrashLog.setup()

                // Auto-upload the previous (potentially crashed) session log to the
                // desktop hub if one exists and the hub URL is configured.
                if !HubProfileSync.orderedHubHttpBases().isEmpty,
                   JarvisIOSCrashLog.previousSessionLogURL != nil {
                    Task {
                        if let result = await JarvisIOSCrashLog.uploadPreviousSessionLogWithFallback() {
                            JarvisIOSLog.recordHub("Prev crash log uploaded: \(result)")
                        }
                    }
                }

                HubProfileSync.warmUpCachedHubEnvironmentIfPossible()
                IronclawConnectivity.shared.start()
            }
    }
}
