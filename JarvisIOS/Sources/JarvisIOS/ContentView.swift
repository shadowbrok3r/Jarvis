import SwiftUI

struct ContentView: View {
    var body: some View {
        MainShellView()
            .onAppear {
                HubProfileSync.warmUpCachedHubEnvironmentIfPossible()
                IronclawConnectivity.shared.start()
            }
    }
}
