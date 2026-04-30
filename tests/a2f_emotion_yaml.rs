//! Parses the YAML emotion fixture used by the Kokoro→A2F pipeline docs/tests.

use std::collections::HashMap;
use std::fs;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct EmotionMap {
    #[serde(flatten)]
    emotions: HashMap<String, f32>,
}

#[test]
fn a2f_emotion_yaml_fixture_loads() {
    let raw = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/a2f_emotions_smoke.yaml"
    ))
    .expect("read fixture");
    let m: EmotionMap = serde_yaml::from_str(&raw).expect("yaml");
    assert!((m.emotions.get("joy").copied().unwrap_or(0.0) - 0.55).abs() < 1e-6);
}
