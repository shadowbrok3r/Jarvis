//! Strip / parse ACT + DELAY tokens emitted by IronClaw-style assistants.
//!
//! Two ACT syntaxes are in the wild:
//!
//! * **pipe form** (AIRI / server.mjs legacy):  `<|ACT:{"emotion":"happy"}|>`
//! * **bracket form** (current IronClaw output, what we actually see today):
//!   `[ACT emotion="sensual"]`, `[ACT emotion=curious]`, `[ ACT emotion="x"]`,
//!   and `[ACT:{"emotion":"happy"}]`.
//!
//! Both forms are parsed here. A separate [`DelayToken`] path handles
//! `<|DELAY:1200|>` and `[DELAY:1200]`. The `strip_act_delay` helper scrubs
//! both forms from any piece of text so the chat transcript + TTS see a
//! clean copy without having to reimplement the regex set.

use std::borrow::Cow;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

use super::types::{ActToken, DelayToken, Emotion};

// ------- Regex bank -----------------------------------------------------------

/// `<|ACT:{...}|>` (pipe form, JSON body only).
static ACT_PIPE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<\|ACT\s*(?::\s*)?(\{[\s\S]*?\})\|>").expect("ACT pipe regex"));

/// `[ACT ...]` / `[ACT:...]` / `[ ACT emotion="x"]` — bracket form, inner can
/// be either a JSON object OR a whitespace-separated attribute list.
///
/// Captures:
///   * group 1 — `{...}` when the caller wrote JSON (`[ACT:{"emotion":"x"}]`)
///   * group 2 — attribute list (`emotion="x" intensity=0.5`) otherwise
///
/// `[\s\S]` is used instead of `.` so ACT tokens that span newlines (rare,
/// but possible when the assistant pretty-prints JSON) still match.
static ACT_BRACKET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?x)                   # ignore whitespace / allow comments
        \[                       # literal [
        \s* ACT \s*              # the tag, any leading whitespace tolerated
        (?: : \s* )?             # optional colon separator
        (?:
            ( \{ [\s\S]*? \} )   # (1) JSON body
          | ( [^\]]*? )          # (2) attribute list (non-greedy, no ])
        )
        \s* \]",
    )
    .expect("ACT bracket regex")
});

/// `<|DELAY:n|>` — millisecond delay, pipe form.
static DELAY_PIPE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<\|DELAY:(\d+)\|>").expect("DELAY pipe regex"));

/// `[DELAY:n]` — millisecond delay, bracket form.
static DELAY_BRACKET_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[\s*DELAY\s*:\s*(\d+)\s*\]").expect("DELAY bracket regex"));

/// Match every ACT bracket/pipe token (no captures). Used by stripping pass.
static ACT_ANY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?x)
        ( <\|ACT\s*(?::\s*)?\{[\s\S]*?\}\|> )
      | ( \[\s*ACT\s*(?::\s*)?(?: \{[\s\S]*?\} | [^\]]*? )\s*\] )",
    )
    .expect("ACT any regex")
});

/// Match every DELAY bracket/pipe token.
static DELAY_ANY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?x) ( <\|DELAY:\d+\|> ) | ( \[\s*DELAY\s*:\s*\d+\s*\] )")
        .expect("DELAY any regex")
});

/// `emotion=sensual` / `emotion = "sensual"` — attribute pair inside a
/// bracket-form ACT body. The value can be quoted (`"..."` / `'...'`) or
/// bare.
static ATTR_PAIR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?x)
        (?P<key> [A-Za-z_][A-Za-z0-9_]* )
        \s* = \s*
        (?:
            " (?P<qd> [^"]* ) "
          | ' (?P<qs> [^']* ) '
          | (?P<bare> [A-Za-z0-9_\-./]+ )
        )"#,
    )
    .expect("attr regex")
});

// ------- Token types ----------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum EitherToken {
    Act(ActToken),
    Delay(DelayToken),
}

// ------- Token collection -----------------------------------------------------

/// Collect ACT / DELAY tokens in document order, across both bracket and
/// pipe syntaxes. Overlapping matches are suppressed.
#[must_use]
pub fn parse_act_tokens(input: &str) -> Vec<EitherToken> {
    #[derive(Clone)]
    struct Hit {
        start: usize,
        end: usize,
        kind: HitKind,
    }

    #[derive(Clone)]
    enum HitKind {
        Act(String),
        Delay(u32),
    }

    let mut hits: Vec<Hit> = Vec::new();

    for m in ACT_PIPE_RE.captures_iter(input) {
        let full = m.get(0).expect("pipe match 0");
        let body = m.get(1).expect("pipe act body");
        hits.push(Hit {
            start: full.start(),
            end: full.end(),
            kind: HitKind::Act(body.as_str().to_string()),
        });
    }

    for m in ACT_BRACKET_RE.captures_iter(input) {
        let full = m.get(0).expect("bracket match 0");
        // Normalise bracket form to a JSON-looking body so downstream helpers
        // can treat both syntaxes identically. Attribute form collapses into
        // `{"emotion":"sensual","intensity":0.5}`.
        let body = if let Some(json) = m.get(1) {
            json.as_str().to_string()
        } else {
            let attrs = m.get(2).map(|s| s.as_str().trim()).unwrap_or("");
            attrs_to_json(attrs)
        };
        hits.push(Hit {
            start: full.start(),
            end: full.end(),
            kind: HitKind::Act(body),
        });
    }

    for m in DELAY_PIPE_RE.captures_iter(input) {
        let full = m.get(0).expect("delay pipe match 0");
        let g1 = m.get(1).expect("delay pipe body");
        let ms: u32 = g1.as_str().parse().unwrap_or(0);
        hits.push(Hit {
            start: full.start(),
            end: full.end(),
            kind: HitKind::Delay(ms),
        });
    }

    for m in DELAY_BRACKET_RE.captures_iter(input) {
        let full = m.get(0).expect("delay bracket match 0");
        let g1 = m.get(1).expect("delay bracket body");
        let ms: u32 = g1.as_str().parse().unwrap_or(0);
        hits.push(Hit {
            start: full.start(),
            end: full.end(),
            kind: HitKind::Delay(ms),
        });
    }

    hits.sort_by_key(|h| h.start);

    let mut out = Vec::new();
    let mut last_end = 0usize;
    for h in hits {
        if h.start < last_end {
            continue;
        }
        match h.kind {
            HitKind::Act(body) => out.push(EitherToken::Act(ActToken { json: body })),
            HitKind::Delay(ms) => out.push(EitherToken::Delay(DelayToken { ms })),
        }
        last_end = h.end;
    }
    out
}

// ------- Emotion extraction ---------------------------------------------------

/// Parse JSON inside ACT for a structured `emotion` field (strict enum
/// match). Kept for the legacy code paths that still want the typed enum —
/// new code should prefer [`emotion_label_from_act_json`] so custom
/// emotions (`sensual`, `flirty`, …) aren't silently dropped.
#[must_use]
pub fn emotion_from_act_json(json: &str) -> Option<Emotion> {
    #[derive(Deserialize)]
    struct ActBody {
        emotion: Option<Emotion>,
    }

    if let Ok(b) = serde_json::from_str::<ActBody>(json) {
        return b.emotion;
    }
    None
}

/// Extract the free-form `emotion` string from a normalized ACT body —
/// works for JSON (`{"emotion":"sensual"}`), attribute-collapsed JSON
/// (produced by the bracket-form path), or bare attribute text
/// (`emotion=sensual` that somehow skipped normalization).
///
/// Returns the lower-cased emotion label, or `None` if the body doesn't
/// carry one.
#[must_use]
pub fn emotion_label_from_act_json(body: &str) -> Option<String> {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(s) = val.get("emotion").and_then(|v| v.as_str()) {
            return Some(s.trim().to_ascii_lowercase());
        }
    }
    for caps in ATTR_PAIR_RE.captures_iter(body) {
        let key = caps.name("key")?.as_str();
        if !key.eq_ignore_ascii_case("emotion") {
            continue;
        }
        let val = caps
            .name("qd")
            .or_else(|| caps.name("qs"))
            .or_else(|| caps.name("bare"))?
            .as_str()
            .trim()
            .to_ascii_lowercase();
        if !val.is_empty() {
            return Some(val);
        }
    }
    None
}

/// Collect every `emotion` label in document order. Useful for dispatchers
/// that want to play N animations sequentially (currently only the first
/// is honoured, but the full list is surfaced for future use).
#[must_use]
pub fn emotion_labels(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in parse_act_tokens(input) {
        if let EitherToken::Act(act) = tok {
            if let Some(label) = emotion_label_from_act_json(&act.json) {
                out.push(label);
            }
        }
    }
    out
}

// ------- Stripping ------------------------------------------------------------

/// Strip ACT + DELAY tokens (both syntaxes) for TTS / transcript display.
/// Also drops single-asterisk emphasis the way AIRI's TTS pipeline does so
/// Kokoro doesn't speak `*sigh*`.
#[must_use]
pub fn strip_act_delay_for_tts(input: &str) -> Cow<'_, str> {
    let cleaned = strip_act_delay(input);
    // Match AIRI-ish markdown stripping for TTS: `*emphasis*`.
    static STAR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\*[^*\n]+\*").expect("star regex"));
    let out = STAR_RE.replace_all(&cleaned, "").trim().to_string();
    Cow::Owned(out)
}

/// Heuristic: gateway/provider failure text that must not be sent to Kokoro (avoids huge bogus WAV/PCM).
/// Same rules as desktop `ironclaw_chat` — keep hub + gateway paths aligned.
pub fn should_skip_tts_for_error_like_response(text: &str) -> bool {
    let head: String = text.trim().chars().take(80).collect();
    let lowered = head.to_ascii_lowercase();
    lowered.starts_with("error:")
        || lowered.starts_with("error -")
        || lowered.starts_with("gateway error")
        || lowered.contains("llm error")
        || (lowered.starts_with("provider ") && lowered.contains(" error"))
}

/// Strip ACT + DELAY tokens (both syntaxes), leaving markdown intact. Use
/// this for the chat transcript so bubbles stop showing `[ACT ...]`
/// without losing `*italic*` styling the way TTS does.
#[must_use]
pub fn strip_act_delay(input: &str) -> Cow<'_, str> {
    let without_act = ACT_ANY_RE.replace_all(input, "");
    let without_all = DELAY_ANY_RE.replace_all(&without_act, "").into_owned();
    // Collapse the double-spaces the removal leaves behind (e.g.
    // `Hello  world` after `Hello <ACT> world`). Keeps single newlines.
    static MULTI_SPACE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t]{2,}").expect("space re"));
    let tidy = MULTI_SPACE
        .replace_all(&without_all, " ")
        .trim()
        .to_string();
    Cow::Owned(tidy)
}

// ------- Sentence chunking (incremental TTS) ----------------------------------

/// Largest single Kokoro request we'll emit before soft-splitting at a clause
/// break. Keeps time-to-first-audio bounded even for a long, punctuation-free
/// run-on sentence.
const MAX_TTS_CHUNK_CHARS: usize = 220;

/// Split an assistant reply into ordered chunks for **incremental TTS**.
///
/// Sending the first sentence to Kokoro while later sentences are still
/// synthesizing is the single biggest cut to time-to-first-audio: playback
/// starts after `synth(sentence_1)` instead of `synth(whole_reply)`. Input is
/// expected to already be ACT/DELAY-stripped (see [`strip_act_delay_for_tts`]).
///
/// Boundary rules:
///   * Break after a run of `.`/`!`/`?`/`…` that is followed by whitespace or
///     end-of-text — so decimals (`3.5`), versions (`0.18`) and URLs stay whole.
///   * Break on newlines (lists / paragraphs are natural pauses).
///   * Don't break after common abbreviations (`Dr.`, `e.g.`) or single-letter
///     initials (`J. R. R.`).
///   * Soft-split any chunk longer than [`MAX_TTS_CHUNK_CHARS`] at the last
///     clause break before the limit.
///
/// Returns chunks in speaking order; never returns empty strings. Uses a manual
/// scan rather than a regex because Rust's `regex` crate has no lookbehind, so
/// the "split on `.` only when followed by a capital" idiom isn't expressible.
#[must_use]
pub fn split_into_tts_sentences(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut raw: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c == '\n' {
            flush_sentence(&mut current, &mut raw);
            i += 1;
            continue;
        }
        current.push(c);
        if matches!(c, '.' | '!' | '?' | '…') {
            // Consume a run of terminal punctuation ("?!", "...", "…").
            while i + 1 < n && matches!(chars[i + 1], '.' | '!' | '?' | '…') {
                i += 1;
                current.push(chars[i]);
            }
            // Real boundary = terminal punctuation, then whitespace (guards
            // decimals like `3.5`), then a char that *starts* a new sentence
            // (uppercase / opening quote / EOS). This keeps trailing-off
            // ellipses joined ("Wait... really?!") while still breaking on a
            // genuine full stop.
            let immediate = chars.get(i + 1).copied();
            let followed_by_space = immediate.is_none_or(char::is_whitespace);
            let next_non_ws = chars[i + 1..].iter().copied().find(|c| !c.is_whitespace());
            let starts_sentence = next_non_ws.is_none_or(is_sentence_start);
            if followed_by_space && starts_sentence && !ends_with_abbreviation(&current) {
                flush_sentence(&mut current, &mut raw);
            }
        }
        i += 1;
    }
    flush_sentence(&mut current, &mut raw);

    let mut out = Vec::with_capacity(raw.len());
    for s in raw {
        soft_split_long_chunk(&s, MAX_TTS_CHUNK_CHARS, &mut out);
    }
    out
}

/// True for a character that can begin a new sentence (so a preceding period is
/// a real full stop, not an abbreviation or decimal point).
fn is_sentence_start(c: char) -> bool {
    c.is_uppercase() || matches!(c, '"' | '\'' | '“' | '‘' | '(' | '[' | '*' | '_' | '¿' | '¡' | '«')
}

/// Push `current` (trimmed) onto `out` if it has any speakable content, then
/// clear it either way.
fn flush_sentence(current: &mut String, out: &mut Vec<String>) {
    let trimmed = current.trim();
    if trimmed.chars().any(char::is_alphanumeric) {
        out.push(trimmed.to_string());
    }
    current.clear();
}

/// True when `s` ends in an abbreviation or single-letter initial whose period
/// is not a sentence terminator. `"no"`/`"am"`/`"us"` are deliberately absent so
/// real sentences ending in those words still split; their dotted forms
/// (`a.m`, `u.s`) are matched instead.
fn ends_with_abbreviation(s: &str) -> bool {
    const ABBREV: &[&str] = &[
        "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "inc", "ltd", "co", "fig",
        "vol", "pp", "e.g", "i.e", "a.m", "p.m", "u.s", "u.k",
    ];
    let trailing: String = s
        .trim_end()
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '.')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let tok = trailing.trim_end_matches('.').to_ascii_lowercase();
    if tok.is_empty() {
        return false;
    }
    // Single-letter initial (e.g. "J", "R") — common in names.
    if tok.chars().filter(|c| c.is_alphanumeric()).count() == 1 {
        return true;
    }
    ABBREV.contains(&tok.as_str())
}

/// Break `s` into <= `max_chars` pieces at the last clause break (`,`/`;`/`:`)
/// or whitespace before the limit, pushing each onto `out`. Always makes
/// forward progress so it can't loop on a break-free run.
fn soft_split_long_chunk(s: &str, max_chars: usize, out: &mut Vec<String>) {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        if !s.trim().is_empty() {
            out.push(s.trim().to_string());
        }
        return;
    }
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + max_chars).min(chars.len());
        if end == chars.len() {
            let piece: String = chars[start..end].iter().collect();
            if !piece.trim().is_empty() {
                out.push(piece.trim().to_string());
            }
            break;
        }
        let window = &chars[start..end];
        let cut = window
            .iter()
            .rposition(|&c| matches!(c, ',' | ';' | ':'))
            .map(|p| start + p + 1)
            .or_else(|| {
                window
                    .iter()
                    .rposition(|c| c.is_whitespace())
                    .map(|p| start + p + 1)
            })
            .filter(|&c| c > start)
            .unwrap_or(end);
        let piece: String = chars[start..cut].iter().collect();
        if !piece.trim().is_empty() {
            out.push(piece.trim().to_string());
        }
        start = cut;
    }
}

// ------- Helpers --------------------------------------------------------------

/// Convert a bracket-form attribute body (`emotion="x" intensity=0.5`) into
/// the JSON envelope the rest of the parser works with
/// (`{"emotion":"x","intensity":"0.5"}`). Unknown / malformed fragments are
/// silently skipped.
fn attrs_to_json(attrs: &str) -> String {
    let mut out = serde_json::Map::new();
    for caps in ATTR_PAIR_RE.captures_iter(attrs) {
        let Some(key) = caps.name("key") else {
            continue;
        };
        let val = caps
            .name("qd")
            .or_else(|| caps.name("qs"))
            .or_else(|| caps.name("bare"))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        out.insert(key.as_str().to_string(), serde_json::Value::String(val));
    }
    serde_json::Value::Object(out).to_string()
}

// ------- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_tts_for_provider_error_like_strings() {
        assert!(should_skip_tts_for_error_like_response(
            "Error: LLM error: Provider timeout"
        ));
        assert!(!should_skip_tts_for_error_like_response("Hello world"));
    }

    #[test]
    fn strips_pipe_act_and_delay() {
        let raw = r#"Hello <|ACT:{"emotion":"happy"}|> world <|DELAY:500|> end"#;
        assert_eq!(strip_act_delay_for_tts(raw).as_ref(), "Hello world end");
    }

    #[test]
    fn strips_bracket_act_and_delay() {
        let raw = r#"Hello [ACT emotion="sensual"] world [DELAY:500] end"#;
        assert_eq!(strip_act_delay_for_tts(raw).as_ref(), "Hello world end");
    }

    #[test]
    fn strips_bracket_act_with_leading_space() {
        let raw = r#"okay [ ACT emotion="curious"] maybe"#;
        assert_eq!(strip_act_delay_for_tts(raw).as_ref(), "okay maybe");
    }

    #[test]
    fn parses_pipe_emotion_json() {
        let t = r#"pre <|ACT:{"emotion":"curious"}|> post"#;
        let tokens = parse_act_tokens(t);
        assert!(matches!(
            tokens.as_slice(),
            [EitherToken::Act(a)] if emotion_from_act_json(&a.json) == Some(Emotion::Curious)
        ));
    }

    #[test]
    fn parses_bracket_attr_emotion() {
        let t = r#"pre [ACT emotion="sensual"] post"#;
        let labels = emotion_labels(t);
        assert_eq!(labels, vec!["sensual".to_string()]);
    }

    #[test]
    fn parses_bracket_bare_emotion() {
        let labels = emotion_labels(r#"[ACT emotion=curious]"#);
        assert_eq!(labels, vec!["curious".to_string()]);
    }

    #[test]
    fn parses_bracket_json_body() {
        let labels = emotion_labels(r#"[ACT:{"emotion":"flirty"}]"#);
        assert_eq!(labels, vec!["flirty".to_string()]);
    }

    #[test]
    fn parse_mixed_order() {
        let raw = r#"a <|DELAY:1|> b [ACT emotion="sad"] c"#;
        let v = parse_act_tokens(raw);
        assert!(matches!(&v[0], EitherToken::Delay(d) if d.ms == 1));
        assert!(matches!(
            &v[1],
            EitherToken::Act(ActToken { json }) if json.contains("sad")
        ));
    }

    #[test]
    fn labels_are_lowercased() {
        let labels = emotion_labels(r#"[ACT emotion="SENSUAL"]"#);
        assert_eq!(labels, vec!["sensual".to_string()]);
    }

    #[test]
    fn splits_basic_sentences() {
        assert_eq!(
            split_into_tts_sentences("Hello there. How are you?"),
            vec!["Hello there.".to_string(), "How are you?".to_string()]
        );
    }

    #[test]
    fn keeps_decimals_and_versions_whole() {
        assert_eq!(
            split_into_tts_sentences("The value is 3.5 today."),
            vec!["The value is 3.5 today.".to_string()]
        );
        assert_eq!(
            split_into_tts_sentences("Bevy 0.18 is great. Indeed."),
            vec!["Bevy 0.18 is great.".to_string(), "Indeed.".to_string()]
        );
    }

    #[test]
    fn does_not_split_after_abbreviations() {
        assert_eq!(
            split_into_tts_sentences("Dr. Smith arrived. Good."),
            vec!["Dr. Smith arrived.".to_string(), "Good.".to_string()]
        );
        assert_eq!(
            split_into_tts_sentences("Use fruit, e.g. apples. Done."),
            vec!["Use fruit, e.g. apples.".to_string(), "Done.".to_string()]
        );
    }

    #[test]
    fn real_word_endings_still_split() {
        // "am"/"no" are NOT treated as abbreviations (only their dotted forms).
        assert_eq!(
            split_into_tts_sentences("I am. You are."),
            vec!["I am.".to_string(), "You are.".to_string()]
        );
    }

    #[test]
    fn collapses_terminal_punctuation_runs() {
        assert_eq!(
            split_into_tts_sentences("Wait... really?! Yes."),
            vec!["Wait... really?!".to_string(), "Yes.".to_string()]
        );
    }

    #[test]
    fn breaks_on_newlines() {
        assert_eq!(
            split_into_tts_sentences("Line one\nLine two"),
            vec!["Line one".to_string(), "Line two".to_string()]
        );
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(split_into_tts_sentences("   ").is_empty());
        assert!(split_into_tts_sentences("...").is_empty());
    }

    #[test]
    fn soft_splits_long_runon() {
        let long = format!("{} and that is the end.", "word ".repeat(80));
        let chunks = split_into_tts_sentences(&long);
        assert!(chunks.len() > 1, "expected long run-on to be soft-split");
        assert!(
            chunks.iter().all(|c| c.chars().count() <= MAX_TTS_CHUNK_CHARS),
            "every chunk must respect the size cap"
        );
        // Reassembling the words must preserve content (whitespace-normalized).
        let rejoined = chunks.join(" ");
        let want: String = long.split_whitespace().collect::<Vec<_>>().join(" ");
        let got: String = rejoined.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(got, want);
    }
}
