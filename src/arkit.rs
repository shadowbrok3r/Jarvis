//! ARKit blendshape → VRM expression mapping.
//!
//! Direct port of [`arkit-to-vrm-map.mjs`][src] from the Node pose-controller.
//! Same weight tables, same clamping, same "relaxed = 1 − Σ emotions" rule —
//! so existing expectations about what faces the avatar makes in response to
//! A2F output continue to hold.
//!
//! The VRM preset surface covers:
//! * Mouth visemes (`aa`, `ee`, `ih`, `oh`, `ou`)
//! * Emotions (`happy`, `sad`, `angry`, `surprised`, `relaxed`)
//! * Eyes (`blinkLeft`, `blinkRight`)
//!
//! [src]: https://github.com/airi/mcp-servers/pose-controller/arkit-to-vrm-map.mjs

use std::collections::HashMap;
use std::sync::LazyLock;

/// One term in a VRM expression formula: `src_blendshape * weight`.
#[derive(Debug, Clone, Copy)]
pub struct MapRule {
    pub src: &'static str,
    pub weight: f32,
}

/// Default ARKit blendshape → VRM expression table (mouth + eyes).
pub static DEFAULT_BLENDSHAPE_MAP: LazyLock<HashMap<&'static str, Vec<MapRule>>> =
    LazyLock::new(|| {
        let mut m: HashMap<&'static str, Vec<MapRule>> = HashMap::new();
        m.insert(
            "aa",
            vec![
                MapRule {
                    src: "JawOpen",
                    weight: 0.8,
                },
                MapRule {
                    src: "MouthLowerDownLeft",
                    weight: 0.1,
                },
                MapRule {
                    src: "MouthLowerDownRight",
                    weight: 0.1,
                },
            ],
        );
        m.insert(
            "oh",
            vec![
                MapRule {
                    src: "MouthFunnel",
                    weight: 0.6,
                },
                MapRule {
                    src: "JawOpen",
                    weight: 0.3,
                },
                MapRule {
                    src: "MouthPucker",
                    weight: 0.1,
                },
            ],
        );
        m.insert(
            "ou",
            vec![
                MapRule {
                    src: "MouthPucker",
                    weight: 0.7,
                },
                MapRule {
                    src: "MouthFunnel",
                    weight: 0.3,
                },
            ],
        );
        m.insert(
            "ee",
            vec![
                MapRule {
                    src: "MouthStretchLeft",
                    weight: 0.3,
                },
                MapRule {
                    src: "MouthStretchRight",
                    weight: 0.3,
                },
                MapRule {
                    src: "MouthSmileLeft",
                    weight: 0.2,
                },
                MapRule {
                    src: "MouthSmileRight",
                    weight: 0.2,
                },
            ],
        );
        m.insert(
            "ih",
            vec![
                MapRule {
                    src: "MouthSmileLeft",
                    weight: 0.4,
                },
                MapRule {
                    src: "MouthSmileRight",
                    weight: 0.4,
                },
                MapRule {
                    src: "MouthUpperUpLeft",
                    weight: 0.1,
                },
                MapRule {
                    src: "MouthUpperUpRight",
                    weight: 0.1,
                },
            ],
        );
        m.insert(
            "blinkLeft",
            vec![MapRule {
                src: "EyeBlinkLeft",
                weight: 1.0,
            }],
        );
        m.insert(
            "blinkRight",
            vec![MapRule {
                src: "EyeBlinkRight",
                weight: 1.0,
            }],
        );
        m
    });

/// Default A2F emotion → VRM emotion expression table.
pub static DEFAULT_EMOTION_MAP: LazyLock<HashMap<&'static str, Vec<MapRule>>> =
    LazyLock::new(|| {
        let mut m: HashMap<&'static str, Vec<MapRule>> = HashMap::new();
        m.insert(
            "happy",
            vec![
                MapRule {
                    src: "joy",
                    weight: 0.7,
                },
                MapRule {
                    src: "cheekiness",
                    weight: 0.3,
                },
            ],
        );
        m.insert(
            "sad",
            vec![
                MapRule {
                    src: "sadness",
                    weight: 0.7,
                },
                MapRule {
                    src: "grief",
                    weight: 0.3,
                },
            ],
        );
        m.insert(
            "angry",
            vec![
                MapRule {
                    src: "anger",
                    weight: 0.8,
                },
                MapRule {
                    src: "disgust",
                    weight: 0.2,
                },
            ],
        );
        m.insert(
            "surprised",
            vec![
                MapRule {
                    src: "amazement",
                    weight: 0.8,
                },
                MapRule {
                    src: "fear",
                    weight: 0.2,
                },
            ],
        );
        m
    });

/// Fold one ARKit blendshape frame into a VRM expression frame, clamping `0..=1`.
pub fn map_arkit_to_vrm(
    arkit: &HashMap<String, f32>,
    overrides: Option<&HashMap<&'static str, Vec<MapRule>>>,
) -> HashMap<String, f32> {
    let map = overrides.unwrap_or(&DEFAULT_BLENDSHAPE_MAP);
    let mut out = HashMap::with_capacity(map.len());
    for (vrm_name, rules) in map.iter() {
        let mut v = 0.0_f32;
        for r in rules {
            v += arkit.get(r.src).copied().unwrap_or(0.0) * r.weight;
        }
        out.insert((*vrm_name).to_string(), v.clamp(0.0, 1.0));
    }
    out
}

/// Fold A2F emotion aggregate into VRM emotion expressions.
///
/// Adds a derived `relaxed = max(0, (1 − Σ happy/sad/angry/surprised) * 0.4)` so the
/// avatar does not look aggressively emotional when inputs are weak.
pub fn map_a2f_emotions_to_vrm(
    a2f: &HashMap<String, f32>,
    overrides: Option<&HashMap<&'static str, Vec<MapRule>>>,
) -> HashMap<String, f32> {
    let map = overrides.unwrap_or(&DEFAULT_EMOTION_MAP);
    let mut out: HashMap<String, f32> = HashMap::with_capacity(map.len() + 1);
    for (vrm_name, rules) in map.iter() {
        let mut v = 0.0_f32;
        for r in rules {
            v += a2f.get(r.src).copied().unwrap_or(0.0) * r.weight;
        }
        out.insert((*vrm_name).to_string(), v.clamp(0.0, 1.0));
    }
    let total: f32 = out.values().sum();
    out.insert("relaxed".to_string(), ((1.0 - total) * 0.4).max(0.0));
    out
}

/// One timestamped blendshape frame as received from A2F.
#[derive(Debug, Clone)]
pub struct ArkitKeyframe {
    pub time_code: f64,
    pub blend_shapes: HashMap<String, f32>,
}

/// One timestamped VRM-expression frame after mapping.
#[derive(Debug, Clone)]
pub struct VrmExpressionKeyframe {
    pub time_code: f64,
    pub expressions: HashMap<String, f32>,
}

/// Convert an A2F keyframe stream into a VRM expression keyframe stream.
pub fn map_keyframes_to_vrm(
    frames: &[ArkitKeyframe],
    overrides: Option<&HashMap<&'static str, Vec<MapRule>>>,
) -> Vec<VrmExpressionKeyframe> {
    frames
        .iter()
        .map(|kf| VrmExpressionKeyframe {
            time_code: kf.time_code,
            expressions: map_arkit_to_vrm(&kf.blend_shapes, overrides),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(pairs: &[(&str, f32)]) -> HashMap<String, f32> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn near(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "expected {a} ≈ {b}");
    }

    #[test]
    fn aa_matches_js_formula() {
        // 0.5 * 0.8 + 0.2 * 0.1 + 0.1 * 0.1 = 0.43
        let arkit = hm(&[
            ("JawOpen", 0.5),
            ("MouthLowerDownLeft", 0.2),
            ("MouthLowerDownRight", 0.1),
        ]);
        let vrm = map_arkit_to_vrm(&arkit, None);
        near(vrm["aa"], 0.43);
    }

    #[test]
    fn blinks_pass_through_clamped() {
        let arkit = hm(&[("EyeBlinkLeft", 0.9), ("EyeBlinkRight", 1.2)]);
        let vrm = map_arkit_to_vrm(&arkit, None);
        near(vrm["blinkLeft"], 0.9);
        near(vrm["blinkRight"], 1.0); // clamped
    }

    #[test]
    fn missing_sources_become_zero() {
        let arkit = HashMap::new();
        let vrm = map_arkit_to_vrm(&arkit, None);
        for k in ["aa", "ee", "ih", "oh", "ou", "blinkLeft", "blinkRight"] {
            near(vrm[k], 0.0);
        }
    }

    #[test]
    fn emotion_relaxed_tracks_inverse_total() {
        let a2f = hm(&[("joy", 1.0), ("anger", 0.0), ("sadness", 0.0)]);
        let vrm = map_a2f_emotions_to_vrm(&a2f, None);
        near(vrm["happy"], 0.7);
        // total = 0.7, relaxed = (1 - 0.7) * 0.4 = 0.12
        near(vrm["relaxed"], 0.12);
    }

    #[test]
    fn emotion_relaxed_floors_at_zero() {
        // Over-saturate everything — sum > 1, relaxed clamped to 0.
        let a2f = hm(&[
            ("joy", 1.0),
            ("cheekiness", 1.0),
            ("anger", 1.0),
            ("disgust", 1.0),
            ("amazement", 1.0),
            ("fear", 1.0),
            ("sadness", 1.0),
            ("grief", 1.0),
        ]);
        let vrm = map_a2f_emotions_to_vrm(&a2f, None);
        assert_eq!(vrm["relaxed"], 0.0);
    }

    #[test]
    fn keyframes_preserve_timecode_and_map_values() {
        let frames = vec![
            ArkitKeyframe {
                time_code: 0.0,
                blend_shapes: hm(&[("JawOpen", 1.0)]),
            },
            ArkitKeyframe {
                time_code: 0.5,
                blend_shapes: hm(&[("EyeBlinkLeft", 1.0)]),
            },
        ];
        let mapped = map_keyframes_to_vrm(&frames, None);
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].time_code, 0.0);
        near(mapped[0].expressions["aa"], 0.8); // 1.0 * 0.8
        assert_eq!(mapped[1].time_code, 0.5);
        near(mapped[1].expressions["blinkLeft"], 1.0);
    }
}
