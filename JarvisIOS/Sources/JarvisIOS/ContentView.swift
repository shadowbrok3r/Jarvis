import SwiftUI

struct ContentView: View {
    var body: some View {
        VStack(spacing: 16) {
            Text("JarvisIOS")
                .font(.title)
            Text(jarvis_ios_version().toString())
                .font(.body.monospaced())
                .multilineTextAlignment(.center)
                .padding()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
