import SwiftUI

// MARK: - Wire format (mirrors `jarvis_avatar::emotions::EmotionMapFile` / `EmotionBinding`)

private struct EmotionMapFileDTO: Codable {
    var mappings: [String: EmotionBindingDTO]
}

private struct EmotionBindingDTO: Codable, Equatable {
    var animation: String?
    var expression: String?
    var expressionWeight: Float?
    var expressionBlend: [String: Float]?
    var looping: Bool?
    var holdSeconds: Float?
    var notes: String?
}

private enum VrmExpressionPresets {
    static let names: [String] = [
        "neutral", "happy", "angry", "sad", "relaxed", "surprised",
        "aa", "ih", "ou", "ee", "oh",
        "blink", "blinkLeft", "blinkRight",
        "lookUp", "lookDown", "lookLeft", "lookRight",
        "thinking",
    ]
}

/// Edits `config/emotions.json` under the active asset root (or Application Support fallback).
struct ActEmotionMapEditorView: View {
    @State private var rows: [EmotionRow] = []
    @State private var status: String = ""
    @State private var loadError: String = ""

    var body: some View {
        List {
            if !loadError.isEmpty {
                Section {
                    Text(loadError)
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }
            Section {
                Text(
                    "Keys are ACT emotion labels (lowercase), e.g. `sensual`, `curious`. " +
                        "`animation` is a pose-library JSON filename relative to your animations folder (same as desktop `EmotionBinding.animation`)."
                )
                .font(.caption2)
                .foregroundStyle(.secondary)
            }
            Section("Mappings") {
                ForEach($rows) { $row in
                    NavigationLink {
                        EmotionRowEditorView(
                            row: $row,
                            animationChoices: HubProfileSync.listDiscoveredAnimationJsonRelativePaths(),
                            expressionChoices: VrmExpressionPresets.names
                        )
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(row.key.isEmpty ? "(new)" : row.key)
                                .font(.headline)
                            Text(rowSummary(row))
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                        .padding(.vertical, 4)
                    }
                }
                .onDelete { rows.remove(atOffsets: $0) }
                Button("Add mapping") {
                    rows.append(EmotionRow(key: "", animationFile: "", expression: "", holdSeconds: 2.5, notes: ""))
                }
            }
            Section {
                Button("Reload from disk") { reloadFromDisk() }
                Button("Save") { saveToDisk() }
                if !status.isEmpty {
                    Text(status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .navigationTitle("ACT → emotion")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { reloadFromDisk() }
    }

    private func rowSummary(_ r: EmotionRow) -> String {
        let a = r.animationFile.trimmingCharacters(in: .whitespacesAndNewlines)
        let e = r.expression.trimmingCharacters(in: .whitespacesAndNewlines)
        if a.isEmpty, e.isEmpty { return "—" }
        if a.isEmpty { return "expr: \(e)" }
        if e.isEmpty { return "anim: \(a)" }
        return "anim: \(a) · expr: \(e)"
    }

    private func reloadFromDisk() {
        loadError = ""
        status = ""
        let url = HubProfileSync.resolvedEmotionsJsonFileURL()
        guard FileManager.default.fileExists(atPath: url.path) else {
            rows = []
            status = "No file yet — add rows and Save (path: \(url.path))."
            return
        }
        do {
            let data = try Data(contentsOf: url)
            let dec = JSONDecoder()
            dec.keyDecodingStrategy = .convertFromSnakeCase
            let file = try dec.decode(EmotionMapFileDTO.self, from: data)
            rows = file.mappings.keys.sorted().map { k in
                let b = file.mappings[k] ?? EmotionBindingDTO()
                return EmotionRow(
                    key: k,
                    animationFile: b.animation ?? "",
                    expression: b.expression ?? "",
                    holdSeconds: Double(b.holdSeconds ?? 2.5),
                    notes: b.notes ?? ""
                )
            }
            status = "Loaded \(rows.count) entr\(rows.count == 1 ? "y" : "ies")."
        } catch {
            loadError = error.localizedDescription
            rows = []
        }
    }

    private func saveToDisk() {
        loadError = ""
        status = ""
        var map: [String: EmotionBindingDTO] = [:]
        for r in rows {
            let k = r.key.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            guard !k.isEmpty else { continue }
            let anim = r.animationFile.trimmingCharacters(in: .whitespacesAndNewlines)
            let expr = r.expression.trimmingCharacters(in: .whitespacesAndNewlines)
            map[k] = EmotionBindingDTO(
                animation: anim.isEmpty ? nil : anim,
                expression: expr.isEmpty ? nil : expr,
                expressionWeight: nil,
                expressionBlend: nil,
                looping: nil,
                holdSeconds: Float(r.holdSeconds),
                notes: r.notes.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : r.notes
            )
        }
        let out = EmotionMapFileDTO(mappings: map)
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        enc.keyEncodingStrategy = .convertToSnakeCase
        do {
            let data = try enc.encode(out)
            let url = HubProfileSync.resolvedEmotionsJsonFileURL()
            HubProfileSync.ensureParentDirectoryExists(for: url)
            try data.write(to: url, options: [.atomic])
            status = "Saved \(map.count) mappings to \(url.path)."
            JarvisIOSLog.recordUI("emotions.json saved keys=\(map.count)")
        } catch {
            loadError = error.localizedDescription
        }
    }
}

private struct EmotionRow: Identifiable, Hashable {
    let id: UUID
    var key: String
    /// Pose-library JSON path/filename (maps to `EmotionBinding.animation` on disk). Not named `animation` — clashes with `View.animation(_:)`.
    var animationFile: String
    var expression: String
    var holdSeconds: Double
    var notes: String

    init(id: UUID = UUID(), key: String, animationFile: String, expression: String, holdSeconds: Double, notes: String) {
        self.id = id
        self.key = key
        self.animationFile = animationFile
        self.expression = expression
        self.holdSeconds = holdSeconds
        self.notes = notes
    }
}

private struct EmotionRowEditorView: View {
    @Binding var row: EmotionRow
    let animationChoices: [String]
    let expressionChoices: [String]

    var body: some View {
        Form {
            Section("ACT key") {
                TextField("emotion label (e.g. sensual)", text: $row.key)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            }
            Section("Animation (pose library JSON)") {
                Picker("Preset file", selection: $row.animationFile) {
                    Text("(none)").tag("")
                    ForEach(animationChoices, id: \.self) { path in
                        Text(path).tag(path)
                    }
                }
                TextField("Or type filename (e.g. curious_tilt.json)", text: $row.animationFile)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            }
            Section("VRM expression") {
                Picker("Preset", selection: $row.expression) {
                    Text("(none)").tag("")
                    ForEach(expressionChoices, id: \.self) { name in
                        Text(name).tag(name)
                    }
                }
                TextField("Or custom preset name", text: $row.expression)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            }
            Section("Timing") {
                Stepper(value: $row.holdSeconds, in: 0.25 ... 30, step: 0.25) {
                    Text("Hold: \(row.holdSeconds, specifier: "%.2f") s")
                }
            }
            Section("Notes") {
                TextField("Optional", text: $row.notes, axis: .vertical)
                    .lineLimit(3 ... 8)
            }
        }
        .navigationTitle("Edit mapping")
        .navigationBarTitleDisplayMode(.inline)
    }
}
