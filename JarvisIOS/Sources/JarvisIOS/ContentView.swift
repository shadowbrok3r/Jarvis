import SwiftUI

struct ContentView: View {
    var body: some View {
        MainShellView()
            .onAppear {
                // Set up persistent crash log FIRST (before Bevy / hub sync)
                // so that any log line from this point is written to the file.
                JarvisIOSCrashLog.setup()

                // Auto-upload the previous (potentially crashed) session log to the
                // desktop hub if one exists and the hub URL is configured.
                let hubURL = UserDefaults.standard.string(forKey: HubProfileSync.userDefaultsBaseURLKey) ?? ""
                if !hubURL.isEmpty, JarvisIOSCrashLog.previousSessionLogURL != nil {
                    Task {
                        if let result = await JarvisIOSCrashLog.uploadPreviousSessionLog(hubBaseURL: hubURL) {
                            JarvisIOSLog.recordHub("Prev crash log uploaded: \(result)")
                        }
                    }
                }

                HubProfileSync.warmUpCachedHubEnvironmentIfPossible()
                IronclawConnectivity.shared.start()
            }
    }
}
