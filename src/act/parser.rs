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
    static STAR_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\*[^*\n]+\*").expect("star regex"));
    let out = STAR_RE.replace_all(&cleaned, "").trim().to_string();
    Cow::Owned(out)
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
    let tidy = MULTI_SPACE.replace_all(&without_all, " ").trim().to_string();
    Cow::Owned(tidy)
}

// ------- Helpers --------------------------------------------------------------

/// Convert a bracket-form attribute body (`emotion="x" intensity=0.5`) into
/// the JSON envelope the rest of the parser works with
/// (`{"emotion":"x","intensity":"0.5"}`). Unknown / malformed fragments are
/// silently skipped.
fn attrs_to_json(attrs: &str) -> String {
    let mut out = serde_json::Map::new();
    for caps in ATTR_PAIR_RE.captures_iter(attrs) {
        let Some(key) = caps.name("key") else { continue };
        let val = caps
            .name("qd")
            .or_else(|| caps.name("qs"))
            .or_else(|| caps.name("bare"))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        out.insert(
            key.as_str().to_string(),
            serde_json::Value::String(val),
        );
    }
    serde_json::Value::Object(out).to_string()
}

// ------- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
