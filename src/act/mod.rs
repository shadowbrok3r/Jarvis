//! ACT / DELAY token parsing (AIRI-compatible).

pub mod parser;
pub mod types;

pub use parser::{
    EitherToken, emotion_from_act_json, emotion_label_from_act_json, emotion_labels,
    parse_act_tokens, should_skip_tts_for_error_like_response, strip_act_delay,
    strip_act_delay_for_tts,
};
pub use types::{ActToken, DelayToken, Emotion};
