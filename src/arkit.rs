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
//! **Phase 1 (A2F richness):** additional ARKit channels from
//! [`crate::a2f::default_blendshape_params`] are folded into the **same** preset
//! names only (no new VRM keys). Brows/cheek/sneer map to emotion presets with
//! documented tradeoffs (e.g. `EyeSquint*` also appears in genuine smiles — we
//! use a low weight into `angry`; `BrowOuterUp*` overlaps happy vs surprised —
//! split between them). `relaxed` is not driven here; it comes from
//! [`map_a2f_emotions_to_vrm`] + merge.
//!
//! [src]: https://github.com/airi/mcp-servers/pose-controller/arkit-to-vrm-map.mjs

use std::collections::{HashMap, HashSet};
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
        // --- Emotion presets from upper / mid face (ARKit names match A2F `default_blendshape_params`) ---
        // happy: cheek/dimple “smile energy” without duplicating viseme `MouthSmile*` weights used by `ih`/`ee`.
        m.insert(
            "happy",
            vec![
                MapRule {
                    src: "CheekSquintLeft",
                    weight: 0.45,
                },
                MapRule {
                    src: "CheekSquintRight",
                    weight: 0.45,
                },
                MapRule {
                    src: "MouthDimpleLeft",
                    weight: 0.4,
                },
                MapRule {
                    src: "MouthDimpleRight",
                    weight: 0.4,
                },
                MapRule {
                    src: "BrowOuterUpLeft",
                    weight: 0.25,
                },
                MapRule {
                    src: "BrowOuterUpRight",
                    weight: 0.25,
                },
            ],
        );
        // sad: downturned mouth; `MouthStretch*` is shared with `ee` viseme — keep weights low to limit double-counting.
        m.insert(
            "sad",
            vec![
                MapRule {
                    src: "MouthFrownLeft",
                    weight: 0.55,
                },
                MapRule {
                    src: "MouthFrownRight",
                    weight: 0.55,
                },
                MapRule {
                    src: "MouthPressLeft",
                    weight: 0.3,
                },
                MapRule {
                    src: "MouthPressRight",
                    weight: 0.3,
                },
                MapRule {
                    src: "MouthStretchLeft",
                    weight: 0.12,
                },
                MapRule {
                    src: "MouthStretchRight",
                    weight: 0.12,
                },
            ],
        );
        // angry: knit brow + sneer; `EyeSquint*` is ambiguous (smile vs glare) — moderate weight into angry only.
        m.insert(
            "angry",
            vec![
                MapRule {
                    src: "BrowDownLeft",
                    weight: 0.55,
                },
                MapRule {
                    src: "BrowDownRight",
                    weight: 0.55,
                },
                MapRule {
                    src: "NoseSneerLeft",
                    weight: 0.45,
                },
                MapRule {
                    src: "NoseSneerRight",
                    weight: 0.45,
                },
                MapRule {
                    src: "EyeSquintLeft",
                    weight: 0.22,
                },
                MapRule {
                    src: "EyeSquintRight",
                    weight: 0.22,
                },
            ],
        );
        // surprised: inner brow raise + eye wide; `BrowOuterUp*` is partially allocated to `happy` above.
        m.insert(
            "surprised",
            vec![
                MapRule {
                    src: "BrowInnerUp",
                    weight: 0.65,
                },
                MapRule {
                    src: "EyeWideLeft",
                    weight: 0.55,
                },
                MapRule {
                    src: "EyeWideRight",
                    weight: 0.55,
                },
                MapRule {
                    src: "BrowOuterUpLeft",
                    weight: 0.15,
                },
                MapRule {
                    src: "BrowOuterUpRight",
                    weight: 0.15,
                },
            ],
        );
        // Augment visemes with secondary A2F channels (additive with existing rules; viseme lip sync unchanged in intent).
        m.get_mut("aa").unwrap().push(MapRule {
            src: "JawForward",
            weight: 0.22,
        });
        m.get_mut("oh").unwrap().push(MapRule {
            src: "CheekPuff",
            weight: 0.14,
        });
        m.get_mut("ou").unwrap().extend([
            MapRule {
                src: "MouthRollLower",
                weight: 0.18,
            },
            MapRule {
                src: "MouthShrugLower",
                weight: 0.12,
            },
        ]);
        m.get_mut("ee").unwrap().extend([
            MapRule {
                src: "MouthRollUpper",
                weight: 0.12,
            },
            MapRule {
                src: "MouthShrugUpper",
                weight: 0.1,
            },
        ]);
        m.get_mut("ih").unwrap().extend([
            MapRule {
                src: "MouthLeft",
                weight: 0.06,
            },
            MapRule {
                src: "MouthRight",
                weight: 0.06,
            },
        ]);
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

/// VRM keys owned by lip-sync visemes; clip-level A2F emotion hints must not overwrite these.
pub const VRM_VISEME_KEYS: &[&str] = &["aa", "ee", "ih", "oh", "ou"];

/// Blinks follow ARKit weights only in this merge path.
pub const VRM_BLINK_KEYS: &[&str] = &["blinkLeft", "blinkRight"];

/// Scale for [`map_a2f_emotions_to_vrm`] when layering onto a **speech** clip after [`map_arkit_to_vrm`].
/// Keeps default `joy` hints from dominating brows while still biasing mood. Visemes are untouched.
pub const A2F_EMOTION_SPEECH_BLEND: f32 = 0.3;

fn emotion_layer_normalized_for_merge(
    a2f: &HashMap<String, f32>,
    emotion_rules: Option<&HashMap<&'static str, Vec<MapRule>>>,
) -> HashMap<String, f32> {
    let mut e = map_a2f_emotions_to_vrm(a2f, emotion_rules);
    let h = *e.get("happy").unwrap_or(&0.0);
    let s = *e.get("sad").unwrap_or(&0.0);
    let a = *e.get("angry").unwrap_or(&0.0);
    let z = *e.get("surprised").unwrap_or(&0.0);
    let sum4 = h + s + a + z;
    if sum4 > 1.0 {
        let inv = 1.0 / sum4;
        e.insert("happy".to_string(), (h * inv).clamp(0.0, 1.0));
        e.insert("sad".to_string(), (s * inv).clamp(0.0, 1.0));
        e.insert("angry".to_string(), (a * inv).clamp(0.0, 1.0));
        e.insert("surprised".to_string(), (z * inv).clamp(0.0, 1.0));
        // Quartet capped to sum 1 → same `relaxed` edge case as an oversaturated emotion hint.
        e.insert("relaxed".to_string(), 0.0);
    }
    e
}

/// Merges **once-per-clip** A2F emotion hints (same key space as gRPC `EmotionWithTimeCode`) into each
/// keyframe after ARKit→VRM mapping.
///
/// **Blend rule:** for every key in `emotion_layer` except visemes ([`VRM_VISEME_KEYS`]) and blinks
/// ([`VRM_BLINK_KEYS`]), `out[k] = min(1, frame[k] + A2F_EMOTION_SPEECH_BLEND * emotion[k])`. Viseme
/// and blink weights are taken **only** from the per-frame ARKit map so lip sync is unchanged.
/// Emotion rows are normalized if `happy+sad+angry+surprised > 1` before scaling, then `relaxed` is
/// recomputed from that capped quartet (same 0.4 factor as [`map_a2f_emotions_to_vrm`]).
pub fn merge_a2f_emotion_hint_into_keyframes(
    keyframes: &mut [VrmExpressionKeyframe],
    a2f_emotion: &HashMap<String, f32>,
    emotion_rules: Option<&HashMap<&'static str, Vec<MapRule>>>,
) {
    if a2f_emotion.is_empty() {
        return;
    }
    let layer = emotion_layer_normalized_for_merge(a2f_emotion, emotion_rules);
    let blend = A2F_EMOTION_SPEECH_BLEND;
    let skip: HashSet<&'static str> = VRM_VISEME_KEYS
        .iter()
        .chain(VRM_BLINK_KEYS.iter())
        .copied()
        .collect();
    for kf in keyframes.iter_mut() {
        for (k, v_em) in &layer {
            if skip.contains(k.as_str()) {
                continue;
            }
            let v_ark = kf.expressions.get(k).copied().unwrap_or(0.0);
            let merged = (v_ark + blend * v_em).clamp(0.0, 1.0);
            kf.expressions.insert(k.clone(), merged);
        }
    }
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
        for k in [
            "aa",
            "ee",
            "ih",
            "oh",
            "ou",
            "blinkLeft",
            "blinkRight",
            "happy",
            "sad",
            "angry",
            "surprised",
        ] {
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
    fn brow_down_maps_to_angry_preset() {
        let arkit = hm(&[("BrowDownLeft", 1.0), ("BrowDownRight", 1.0)]);
        let vrm = map_arkit_to_vrm(&arkit, None);
        near(vrm["angry"], (0.55_f32 + 0.55).min(1.0));
    }

    #[test]
    fn merge_skips_visemes_adds_happy() {
        let frames = vec![ArkitKeyframe {
            time_code: 0.0,
            blend_shapes: hm(&[("JawOpen", 1.0)]),
        }];
        let mut vrm = map_keyframes_to_vrm(&frames, None);
        let aa_before = vrm[0].expressions["aa"];
        merge_a2f_emotion_hint_into_keyframes(
            &mut vrm,
            &hm(&[("joy", 1.0)]),
            None,
        );
        near(vrm[0].expressions["aa"], aa_before);
        // happy from hint: 0.7 * 0.3 = 0.21 added to ARKit happy (0 here)
        near(vrm[0].expressions["happy"], 0.21);
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
