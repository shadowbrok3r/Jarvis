import Foundation

/// Minimal Kokoro FastAPI client — mirrors desktop `kokoro_http::fetch_kokoro_speech` (`stream: false`).
enum KokoroSpeechClient {
    private static func normalizedBase(_ base: String) -> String {
        var s = base.trimmingCharacters(in: .whitespacesAndNewlines)
        while s.hasSuffix("/") { s.removeLast() }
        return s
    }

    /// One-shot WAV (`response_format: wav`, `stream: false`) for `AVAudioPlayer`.
    static func fetchWav(baseURL: String, voice: String, text: String) async throws -> Data {
        let b = normalizedBase(baseURL)
        guard let url = URL(string: b + "/v1/audio/speech") else {
            throw URLError(.badURL)
        }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.timeoutInterval = 120
        let body: [String: Any] = [
            "model": "kokoro",
            "voice": voice,
            "input": text,
            "response_format": "wav",
            "stream": false,
        ]
        req.httpBody = try JSONSerialization.data(withJSONObject: body, options: [])
        let (data, resp) = try await URLSession.shared.data(for: req)
        guard let http = resp as? HTTPURLResponse else {
            throw URLError(.badServerResponse)
        }
        guard (200 ... 299).contains(http.statusCode) else {
            let preview = String(data: data.prefix(200), encoding: .utf8) ?? ""
            throw NSError(
                domain: "KokoroSpeechClient",
                code: http.statusCode,
                userInfo: [NSLocalizedDescriptionKey: "Kokoro HTTP \(http.statusCode): \(preview)"]
            )
        }
        return data
    }
}
