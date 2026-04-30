import Foundation

/// Mirrors `jarvis_avatar::act::strip_act_delay` + emotion label extraction for iOS gateway chat.
enum IosChatFormatting {
    // ACT pipe: <|ACT:{...}|>
    private static let actPipePattern = #"(?s)<\|ACT\s*(?::\s*)?(\{.*?\})\|>"#
    // ACT bracket (JSON or attrs)
    private static let actBracketPattern = #"(?s)\[\s*ACT\s*(?::\s*)?(?:(\{.*?\})|([^\]]*?))\s*\]"#
    // DELAY both forms
    private static let delayPattern = #"(?s)(<\|DELAY:\d+\|>)|(\[\s*DELAY\s*:\s*\d+\s*\])"#
    private static let multiSpacePattern = #"[ \t]{2,}"#

    private static func re(_ pattern: String) -> NSRegularExpression {
        // swiftlint:disable:next force_try
        try! NSRegularExpression(pattern: pattern, options: [])
    }

    /// Strip ACT + DELAY tokens; collapse runs of spaces/tabs (desktop `strip_act_delay`).
    static func stripActDelay(_ input: String) -> String {
        var s = input
        s = replaceAll(re(actPipePattern), in: s, with: "")
        s = replaceAll(re(actBracketPattern), in: s, with: "")
        s = replaceAll(re(delayPattern), in: s, with: "")
        s = replaceAll(re(multiSpacePattern), in: s, with: " ")
        return s.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Emotion labels in document order (lowercased, de-duplicated), from ACT bodies only.
    static func emotionLabels(from raw: String) -> [String] {
        var labels: [String] = []
        collectFromActs(re(actPipePattern), in: raw, group: 1, into: &labels)
        let ns = raw as NSString
        let full = NSRange(location: 0, length: ns.length)
        re(actBracketPattern).enumerateMatches(in: raw, options: [], range: full) { result, _, _ in
            guard let r = result else { return }
            let g1 = r.range(at: 1)
            let g2 = r.range(at: 2)
            let body: String
            if g1.location != NSNotFound, g1.length > 0 {
                body = ns.substring(with: g1).trimmingCharacters(in: .whitespacesAndNewlines)
            } else if g2.location != NSNotFound, g2.length > 0 {
                body = ns.substring(with: g2).trimmingCharacters(in: .whitespacesAndNewlines)
            } else {
                return
            }
            if let em = emotionFromActBody(body), !em.isEmpty {
                labels.append(em)
            }
        }
        var out: [String] = []
        var seen = Set<String>()
        for e in labels where !seen.contains(e) {
            out.append(e)
            seen.insert(e)
        }
        return out
    }

    private static func collectFromActs(
        _ regex: NSRegularExpression,
        in s: String,
        group: Int,
        into labels: inout [String]
    ) {
        let ns = s as NSString
        let full = NSRange(location: 0, length: ns.length)
        regex.enumerateMatches(in: s, options: [], range: full) { result, _, _ in
            guard let r = result, r.numberOfRanges > group else { return }
            let gr = r.range(at: group)
            guard gr.location != NSNotFound, gr.length > 0 else { return }
            let body = ns.substring(with: gr).trimmingCharacters(in: .whitespacesAndNewlines)
            if let em = emotionFromActBody(body), !em.isEmpty {
                labels.append(em)
            }
        }
    }

    private static func emotionFromActBody(_ body: String) -> String? {
        let t = body.trimmingCharacters(in: .whitespacesAndNewlines)
        if t.hasPrefix("{"), let d = t.data(using: .utf8),
           let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
           let em = o["emotion"] as? String
        {
            return em.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        }
        // Attribute list: emotion="x" or emotion=x
        let attr = #"(?i)emotion\s*=\s*(?:"([^"]*)"|'([^']*)'|([A-Za-z0-9_\-./]+))"#
        guard let re = try? NSRegularExpression(pattern: attr, options: []),
              let m = re.firstMatch(in: t, options: [], range: NSRange(location: 0, length: (t as NSString).length))
        else { return nil }
        let ns = t as NSString
        if m.range(at: 1).location != NSNotFound {
            return ns.substring(with: m.range(at: 1)).lowercased()
        }
        if m.range(at: 2).location != NSNotFound {
            return ns.substring(with: m.range(at: 2)).lowercased()
        }
        if m.range(at: 3).location != NSNotFound {
            return ns.substring(with: m.range(at: 3)).lowercased()
        }
        return nil
    }

    private static func replaceAll(_ regex: NSRegularExpression, in s: String, with template: String) -> String {
        let ns = s as NSString
        let full = NSRange(location: 0, length: ns.length)
        return regex.stringByReplacingMatches(in: s, options: [], range: full, withTemplate: template)
    }

    private static let starEmphasisPattern = #"\*[^*\n]+\*"#

    /// Desktop `strip_act_delay_for_tts`: ACT/DELAY strip + `*emphasis*` removal for Kokoro input.
    static func stripForKokoro(_ raw: String) -> String {
        let s = stripActDelay(raw)
        let re = re(starEmphasisPattern)
        let stripped = replaceAll(re, in: s, with: "")
        return stripped.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: - Gateway inline images (markdown / data URLs)

    /// Strips `![](data:image/...;base64,...)` and bare `data:image/...;base64,...`; returns cleaned text and `(mime, data)` pairs.
    static func stripGatewayInlineImages(from text: String) -> (String, [(String, Data)]) {
        var s = text
        var collected: [(String, Data)] = []
        let mdPattern = #"!\[[^\]]*\]\(\s*(data:image/(?:png|jpeg|jpg|webp|gif);base64,([A-Za-z0-9+/=]+))\s*\)"#
        if let re = try? NSRegularExpression(pattern: mdPattern, options: []) {
            while true {
                let range = NSRange(location: 0, length: (s as NSString).length)
                guard let m = re.firstMatch(in: s, options: [], range: range), m.numberOfRanges >= 3 else { break }
                let ns = s as NSString
                let full = m.range(at: 0)
                let dataUrl = ns.substring(with: m.range(at: 1))
                let b64 = ns.substring(with: m.range(at: 2))
                if let sep = dataUrl.range(of: ";base64,") {
                    let mimePart = String(dataUrl[..<sep.lowerBound])
                    if mimePart.hasPrefix("data:image/"),
                       let data = Data(base64Encoded: b64, options: [.ignoreUnknownCharacters])
                    {
                        let mime = String(mimePart.dropFirst("data:".count))
                        collected.append((mime, data))
                    }
                }
                let mut = NSMutableString(string: s)
                mut.replaceCharacters(in: full, with: "")
                s = mut as String
            }
        }
        let barePattern = #"data:image/(?:png|jpeg|jpg|webp|gif);base64,[A-Za-z0-9+/=]+"#
        if let re2 = try? NSRegularExpression(pattern: barePattern, options: []) {
            while true {
                let range = NSRange(location: 0, length: (s as NSString).length)
                guard let m = re2.firstMatch(in: s, options: [], range: range) else { break }
                let ns = s as NSString
                let full = m.range(at: 0)
                let span = ns.substring(with: full)
                if let sep = span.range(of: ";base64,") {
                    let mimePart = String(span[..<sep.lowerBound])
                    if mimePart.hasPrefix("data:image/"),
                       let data = Data(base64Encoded: String(span[sep.upperBound...]), options: [.ignoreUnknownCharacters])
                    {
                        let mime = String(mimePart.dropFirst("data:".count))
                        collected.append((mime, data))
                    }
                }
                let mut = NSMutableString(string: s)
                mut.replaceCharacters(in: full, with: "")
                s = mut as String
            }
        }
        return (s.trimmingCharacters(in: .whitespacesAndNewlines), collected)
    }
}
