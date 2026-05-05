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

/// Common VRM0 / VRM1 preset names — use "Or custom" for anything else on the model.
private enum VrmExpressionPresets {
    static let names: [String] = [
        "neutral", "happy", "angry", "sad", "relaxed", "surprised",
        "aa", "ih", "ou", "ee", "oh",
        "blink", "blinkLeft", "blinkRight",
        "lookUp", "lookDown", "lookLeft", "lookRight",
        "thinking",
        "kitagawa", "wink", "kiss", "embarrassed", "joy", "sorrow", "fun", "scared", "disgusted",
        "lookUpLookDown", "lookLeftLookRight",
    ]
}

private enum LoopingMode: String, CaseIterable, Identifiable {
    case inherit
    case loop
    case noLoop

    var id: String { rawValue }

    var label: String {
        switch self {
        case .inherit: return "Default (from animation JSON)"
        case .loop: return "Loop"
        case .noLoop: return "Play once"
        }
    }

    init(from optional: Bool?) {
        switch optional {
        case nil: self = .inherit
        case true?: self = .loop
        case false?: self = .noLoop
        }
    }

    var jsonValue: Bool? {
        switch self {
        case .inherit: return nil
        case .loop: return true
        case .noLoop: return false
        }
    }
}

private struct ExpressionBlendRow: Identifiable, Hashable {
    let id: UUID
    var preset: String
    var weight: Double

    init(id: UUID = UUID(), preset: String = "", weight: Double = 0.5) {
        self.id = id
        self.preset = preset
        self.weight = weight
    }
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
                        "Matches desktop `config/emotions.json`: primary expression + weight, optional multi-preset blend, looping, animation path."
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
                    rows.append(EmotionRow())
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
        let blend = r.blendRows.filter { !$0.preset.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        var parts: [String] = []
        if !a.isEmpty { parts.append("anim: \(a)") }
        if !e.isEmpty {
            let w = r.expressionWeight
            parts.append(abs(w - 1.0) < 0.001 ? "expr: \(e)" : "expr: \(e) @ \(String(format: "%.2f", w))")
        }
        if !blend.isEmpty { parts.append("blend: \(blend.count) preset(s)") }
        if parts.isEmpty { return "—" }
        return parts.joined(separator: " · ")
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
                let blendSorted = (b.expressionBlend ?? [:]).sorted { $0.key < $1.key }.map { kv in
                    ExpressionBlendRow(preset: kv.key, weight: Double(kv.value))
                }
                let w = b.expressionWeight.map(Double.init) ?? 1.0
                return EmotionRow(
                    key: k,
                    animationFile: b.animation ?? "",
                    expression: b.expression ?? "",
                    expressionWeight: w,
                    loopingMode: LoopingMode(from: b.looping),
                    blendRows: blendSorted,
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
            var blendOut: [String: Float]?
            var merged: [String: Float] = [:]
            for br in r.blendRows {
                let pk = br.preset.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !pk.isEmpty else { continue }
                merged[pk] = Float(min(max(br.weight, 0), 1))
            }
            if !merged.isEmpty {
                blendOut = merged
            }
            let ew: Float? = expr.isEmpty ? nil : Float(min(max(r.expressionWeight, 0), 1))
            map[k] = EmotionBindingDTO(
                animation: anim.isEmpty ? nil : anim,
                expression: expr.isEmpty ? nil : expr,
                expressionWeight: ew,
                expressionBlend: blendOut,
                looping: r.loopingMode.jsonValue,
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
    var expressionWeight: Double
    var loopingMode: LoopingMode
    var blendRows: [ExpressionBlendRow]
    var holdSeconds: Double
    var notes: String

    init(
        id: UUID = UUID(),
        key: String = "",
        animationFile: String = "",
        expression: String = "",
        expressionWeight: Double = 1.0,
        loopingMode: LoopingMode = .inherit,
        blendRows: [ExpressionBlendRow] = [],
        holdSeconds: Double = 2.5,
        notes: String = ""
    ) {
        self.id = id
        self.key = key
        self.animationFile = animationFile
        self.expression = expression
        self.expressionWeight = expressionWeight
        self.loopingMode = loopingMode
        self.blendRows = blendRows
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
                Picker("Looping override", selection: $row.loopingMode) {
                    ForEach(LoopingMode.allCases) { m in
                        Text(m.label).tag(m)
                    }
                }
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
                Stepper(value: $row.expressionWeight, in: 0 ... 1, step: 0.05) {
                    Text("Primary weight: \(row.expressionWeight, specifier: "%.2f")")
                }
                .disabled(row.expression.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            Section("Expression blend (optional)") {
                Text("Extra preset → weight pairs merged with the primary expression (same as desktop `expression_blend`).")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                ForEach($row.blendRows) { $br in
                    HStack(alignment: .firstTextBaseline) {
                        TextField("preset", text: $br.preset)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        Stepper(value: $br.weight, in: 0 ... 1, step: 0.05) {
                            Text(String(format: "%.2f", br.weight))
                                .monospacedDigit()
                        }
                    }
                }
                .onDelete { row.blendRows.remove(atOffsets: $0) }
                Button("Add blend slot") {
                    row.blendRows.append(ExpressionBlendRow())
                }
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
