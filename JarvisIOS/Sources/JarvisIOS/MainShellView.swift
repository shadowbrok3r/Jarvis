import SwiftUI

/// Root navigation: chat-first companion, Bevy avatar tab (`xtool` + `RustLibs/libjarvis_ios.a`), link status.
struct MainShellView: View {
    @State private var selectedTab: JarvisTab = .chat

    var body: some View {
        TabView(selection: $selectedTab) {
            ChatTabView()
                .tabItem { Label("Chat", systemImage: "bubble.left.and.bubble.right") }
                .tag(JarvisTab.chat)

            AvatarTabView()
                .tabItem { Label("Avatar", systemImage: "person.crop.artframe") }
                .tag(JarvisTab.avatar)

            LinkTabView()
                .tabItem { Label("Link", systemImage: "link.circle") }
                .tag(JarvisTab.link)
        }
    }
}

private enum JarvisTab: Hashable {
    case chat, avatar, link
}

// MARK: - Chat (gateway / IronClaw wiring comes next)

struct ChatTabView: View {
    @State private var draft = ""
    @State private var lines: [String] = [
        "Chat UI placeholder — connect to the same gateway / IronClaw channel as desktop Jarvis.",
    ]

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                List(lines, id: \.self) { line in
                    Text(line)
                        .font(.body)
                        .textSelection(.enabled)
                }
                .listStyle(.plain)

                HStack {
                    TextField("Message…", text: $draft, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .lineLimit(1 ... 4)
                    Button("Send") {
                        let t = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                        guard !t.isEmpty else { return }
                        lines.append(t)
                        draft = ""
                    }
                    .buttonStyle(.borderedProminent)
                }
                .padding()
            }
            .navigationTitle("Jarvis")
        }
    }
}

// MARK: - Avatar (Bevy in `jarvis_ios`, built for device via `./scripts/build-rust.sh` + xtool)

struct AvatarTabView: View {
    var body: some View {
        NavigationStack {
            ZStack(alignment: .top) {
                JarvisBevyView()
                    .ignoresSafeArea(edges: .bottom)

                Text("Bevy demo scene — add VRM via `bevy_vrm1` next. Use `xtool dev` after `./scripts/build-rust.sh`.")
                    .font(.caption2)
                    .multilineTextAlignment(.center)
                    .padding(8)
                    .frame(maxWidth: .infinity)
                    .background(.ultraThinMaterial)
                    .allowsHitTesting(false)
            }
            .navigationTitle("Avatar")
        }
    }
}

// MARK: - Link (phone as virtual HA-style device for desktop)

struct LinkTabView: View {
    var body: some View {
        NavigationStack {
            Form {
                Section("Rust bridge") {
                    LabeledContent("Build") {
                        Text(jarvis_ios_version().toString())
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                    }
                }
                Section("Desktop parity") {
                    Text(
                        "Expose camera frames, mic audio, and speaker playback over the same logical channel desktop uses for Home Assistant entities (eyes / ears / voice). Discovery on desktop should list this phone as a selectable device alongside HA cameras and assist satellites."
                    )
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Link")
        }
    }
}
