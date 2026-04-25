use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelayToken {
    pub ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActToken {
    pub json: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Emotion {
    Happy,
    Sad,
    Angry,
    Think,
    #[serde(rename = "surprised")]
    Surprised,
    Awkward,
    Question,
    Curious,
    Neutral,
}

impl Emotion {
    /// Maps to the VRM 1.0 expression preset name (best-effort).
    #[must_use]
    pub fn vrm_expression_name(&self) -> &'static str {
        match self {
            Emotion::Happy => "happy",
            Emotion::Sad => "sad",
            Emotion::Angry => "angry",
            Emotion::Think => "thinking",
            Emotion::Surprised => "surprised",
            Emotion::Awkward => "neutral",
            Emotion::Question => "thinking",
            Emotion::Curious => "thinking",
            Emotion::Neutral => "neutral",
        }
    }

    /// Body motion cue label (VRMA / animation naming — placeholder until wired).
    #[must_use]
    pub fn motion_name(&self) -> &'static str {
        match self {
            Emotion::Happy => "Happy",
            Emotion::Sad => "Sad",
            Emotion::Angry => "Angry",
            Emotion::Think => "Think",
            Emotion::Surprised => "Surprise",
            Emotion::Awkward => "Awkward",
            Emotion::Question => "Question",
            Emotion::Neutral => "Idle",
            Emotion::Curious => "Curious",
        }
    }
}
