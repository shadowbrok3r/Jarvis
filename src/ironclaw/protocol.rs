//! envelope: `{ "json": { "type", "data", "metadata" }, "meta": {} }`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperEnvelope {
    pub json: EnvelopeBody,
    #[serde(default)]
    pub meta: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeBody {
    #[serde(rename = "type")]
    pub message_type: String,
    pub data: Value,
    #[serde(default)]
    pub metadata: Value,
}

impl SuperEnvelope {
    /// Best-effort parse of a text WebSocket payload.
    pub fn parse_str(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}
