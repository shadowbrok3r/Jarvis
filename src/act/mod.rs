//! ACT / DELAY token parsing (AIRI-compatible).

pub mod parser;
pub mod types;

pub use parser::{
    emotion_from_act_json, emotion_label_from_act_json, emotion_labels, parse_act_tokens,
    strip_act_delay, strip_act_delay_for_tts, EitherToken,
};
pub use types::{ActToken, DelayToken, Emotion};
