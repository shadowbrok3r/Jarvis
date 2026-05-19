import SwiftUI

enum AvatarBottomPanelTab: String, CaseIterable, Identifiable {
    case chat
    case expressions
    case motion
    case layers

    var id: String { rawValue }

    var title: String {
        switch self {
        case .chat: return "Chat"
        case .expressions: return "Expressions"
        case .motion: return "Motion"
        case .layers: return "Layers"
        }
    }

    var systemImage: String {
        switch self {
        case .chat: return "bubble.left.and.text.bubble"
        case .expressions: return "face.smiling"
        case .motion: return "figure.walk"
        case .layers: return "square.stack.3d.up"
        }
    }
}

/// Avatar-tab bottom tools: chat + expressions + motion (idle + clips) + procedural layers.
struct AvatarToolsOverlay: View {
    @Binding var selectedTab: AvatarBottomPanelTab
    @Binding var panelHeight: CGFloat
    let viewportHeight: CGFloat
    let onDismiss: () -> Void
    @Bindable var chatModel: GatewayChatViewModel

    @AppStorage(HubProfileSync.IosAvatarCustomize.userDefaultsIdleVrmaRelPathOverrideKey) private var idleOverrideRel: String = ""

    private var minPanelHeight: CGFloat { 160 }
    private var maxPanelHeight: CGFloat { max(minPanelHeight, viewportHeight * 0.82) }

    var body: some View {
        AvatarResizableBottomPanel(
            height: $panelHeight,
            minHeight: minPanelHeight,
            maxHeight: maxPanelHeight,
            header: {
                HStack {
                    Picker("Panel", selection: $selectedTab) {
                        ForEach(AvatarBottomPanelTab.allCases) { tab in
                            Text(tab.title).tag(tab)
                        }
                    }
                    .pickerStyle(.segmented)

                    Button {
                        onDismiss()
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.title3)
                            .symbolRenderingMode(.hierarchical)
                    }
                    .accessibilityLabel("Close panel")
                }
            },
            content: {
                Group {
                    switch selectedTab {
                    case .chat:
                        GatewayChatView(model: chatModel, compact: true, onDismissCompact: onDismiss)
                    case .expressions:
                        AvatarExpressionsPanelView()
                    case .motion:
                        AvatarMotionPanelView(idleOverrideRel: $idleOverrideRel)
                    case .layers:
                        AvatarLayersPanelView()
                    }
                }
                .padding(.horizontal, 4)
                .padding(.bottom, 8)
                .transaction { $0.animation = nil }
            }
        )
        .padding(.horizontal, 10)
        .padding(.bottom, 8)
    }
}

// MARK: - Expressions

private struct ExpressionPresetRow: Identifiable {
    let name: String
    var weight: Float
    var id: String { name }
}

private struct AvatarExpressionsPanelView: View {
    @State private var presets: [ExpressionPresetRow] = []
    @State private var status: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if !status.isEmpty {
                Text(status).font(.caption).foregroundStyle(.secondary)
            }
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach($presets) { $row in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(row.name)
                                .font(.caption)
                                .lineLimit(2)
                            Slider(
                                value: Binding(
                                    get: { Double(row.weight) },
                                    set: { newVal in
                                        row.weight = Float(newVal)
                                        JarvisBevySession.setExpressionWeight(name: row.name, weight: row.weight)
                                    }
                                ),
                                in: 0 ... 1
                            )
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .scrollIndicators(.visible)
            .scrollBounceBehavior(.basedOnSize)

            HStack {
                Button("Reset all") {
                    for i in presets.indices {
                        presets[i].weight = 0
                        JarvisBevySession.setExpressionWeight(name: presets[i].name, weight: 0)
                    }
                    JarvisBevySession.applyExpressions()
                }
                Spacer()
                Button("Refresh") { refreshFromRenderer() }
            }
            .buttonStyle(.bordered)
        }
        .onAppear { refreshFromRenderer() }
    }

    private func refreshFromRenderer() {
        guard let data = JarvisBevySession.expressionsSnapshotJSON().data(using: .utf8) else { return }
        struct Snap: Decodable {
            struct Row: Decodable { let name: String; let weight: Float }
            let presets: [Row]
        }
        guard let snap = try? JSONDecoder().decode(Snap.self, from: data) else {
            status = "Waiting for VRM…"
            return
        }
        status = snap.presets.isEmpty ? "No expression presets yet." : "\(snap.presets.count) presets"
        presets = snap.presets.map { ExpressionPresetRow(name: $0.name, weight: $0.weight) }
    }
}

// MARK: - Motion (idle + clips)

private struct AvatarMotionPanelView: View {
    @Binding var idleOverrideRel: String
    @AppStorage("jarvis.ios.phoneSpringGravity") private var phoneSpringGravity = true
    @State private var vrmaPaths: [String] = []
    @State private var jsonPaths: [String] = []
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Group {
                    Text("Idle loop (VRMA)")
                        .font(.headline)
                    Text("Loops on profile reload. Hub manifest idle is used when override is empty.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Picker("Idle VRMA", selection: $idleOverrideRel) {
                        Text("Default (manifest)").tag("")
                        ForEach(vrmaPaths, id: \.self) { path in
                            Text(path).tag(path)
                        }
                    }
                    .pickerStyle(.menu)

                    HStack {
                        Button("Apply idle & reload") {
                            HubProfileSync.applyIosAvatarOverrideEnvFromUserDefaults()
                            JarvisBevySession.reloadProfileFromDiskManifest()
                        }
                        .buttonStyle(.borderedProminent)
                        Button("Clear override") {
                            idleOverrideRel = ""
                            HubProfileSync.applyIosAvatarOverrideEnvFromUserDefaults()
                            JarvisBevySession.reloadProfileFromDiskManifest()
                        }
                        .buttonStyle(.bordered)
                    }
                }

                Divider()

                Toggle("Phone motion → spring hair / cloth", isOn: $phoneSpringGravity)
                    .onChange(of: phoneSpringGravity) { _, on in
                        JarvisDeviceMotion.shared.enabled = on
                    }
                Text("Tilt for spring hair/cloth gravity; shake for extra bounce. Humanoid bones are not moved — VRMC springs only.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)

                DeviceMotionLevelView(motion: JarvisDeviceMotion.shared)

                Divider()

                if !vrmaPaths.isEmpty {
                    Text("VRMA clips").font(.headline)
                    ForEach(vrmaPaths, id: \.self) { path in
                        HStack {
                            Text(path).font(.caption).lineLimit(2)
                            Spacer()
                            Button("Play") { JarvisBevySession.queueVrma(path: path, loopForever: false) }
                                .buttonStyle(.bordered)
                            Button("Loop") { JarvisBevySession.queueVrma(path: path, loopForever: true) }
                                .buttonStyle(.bordered)
                        }
                    }
                }

                if !jsonPaths.isEmpty {
                    Text("Pose-library JSON").font(.headline)
                    ForEach(jsonPaths, id: \.self) { path in
                        HStack {
                            Text(path).font(.caption).lineLimit(2)
                            Spacer()
                            Button("Play") { JarvisBevySession.queueAnimJson(path: path, loopForever: false) }
                                .buttonStyle(.bordered)
                            Button("Loop") { JarvisBevySession.queueAnimJson(path: path, loopForever: true) }
                                .buttonStyle(.bordered)
                        }
                    }
                }

                if vrmaPaths.isEmpty && jsonPaths.isEmpty {
                    Text("No animations under the asset root. Run hub sync from About.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 8)
        }
        .scrollBounceBehavior(.basedOnSize)
        .onAppear {
            reloadLists()
            JarvisDeviceMotion.shared.enabled = phoneSpringGravity
            JarvisBevySession.pushDeviceMotionTuning()
        }
    }

    private func reloadLists() {
        vrmaPaths = HubProfileSync.listDiscoveredVrmaRelativePaths()
        jsonPaths = HubProfileSync.listDiscoveredAnimationJsonRelativePaths()
    }
}

// MARK: - Layers

private struct LayerRowModel: Identifiable {
    let id: UInt64
    let label: String
    let kind: String
    var enabled: Bool
    var weight: Float
}

private struct AvatarLayersPanelView: View {
    @State private var masterEnabled = true
    @State private var layers: [LayerRowModel] = []
    @State private var status: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Toggle("Master (compose layers)", isOn: $masterEnabled)
                .onChange(of: masterEnabled) { _, on in
                    JarvisBevySession.layersSetMaster(enabled: on)
                }

            HStack {
                Button("Install default procedural") {
                    JarvisBevySession.layersInstallDefault()
                    refresh()
                }
                .buttonStyle(.borderedProminent)
                Button("Clear all") {
                    JarvisBevySession.layersClear()
                    refresh()
                }
                .buttonStyle(.bordered)
            }

            if !status.isEmpty {
                Text(status).font(.caption).foregroundStyle(.secondary)
            }

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach($layers) { $row in
                        VStack(alignment: .leading, spacing: 6) {
                            Toggle(isOn: $row.enabled) {
                                VStack(alignment: .leading) {
                                    Text(row.label).font(.subheadline)
                                    Text(row.kind).font(.caption2).foregroundStyle(.secondary)
                                }
                            }
                            .onChange(of: row.enabled) { _, on in
                                JarvisBevySession.layersSetEnabled(layerId: row.id, enabled: on)
                            }
                            Slider(
                                value: Binding(
                                    get: { Double(row.weight) },
                                    set: { v in
                                        row.weight = Float(v)
                                        JarvisBevySession.layersSetWeight(layerId: row.id, weight: row.weight)
                                    }
                                ),
                                in: 0 ... 1
                            ) {
                                Text("Weight")
                            }
                        }
                        .padding(.vertical, 4)
                    }
                }
            }
            .scrollBounceBehavior(.basedOnSize)

            Button("Refresh") { refresh() }
                .buttonStyle(.bordered)
        }
        .padding(.horizontal, 8)
        .onAppear { refresh() }
    }

    private func refresh() {
        guard let data = JarvisBevySession.layersSnapshotJSON().data(using: .utf8) else { return }
        struct Snap: Decodable {
            struct Row: Decodable {
                let id: UInt64
                let label: String
                let kind: String
                let enabled: Bool
                let weight: Float
            }
            let masterEnabled: Bool
            let layers: [Row]
        }
        guard let snap = try? JSONDecoder().decode(Snap.self, from: data) else { return }
        masterEnabled = snap.masterEnabled
        layers = snap.layers.map {
            LayerRowModel(id: $0.id, label: $0.label, kind: $0.kind, enabled: $0.enabled, weight: $0.weight)
        }
        status = layers.isEmpty ? "No layers — tap Install default." : "\(layers.count) layer(s)"
    }
}
