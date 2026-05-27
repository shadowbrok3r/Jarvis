//! DTOs for the ZeroClaw gateway (HTTP + WS + SSE).
//!
//! Kept intentionally thin — only the fields the Rust client reads/writes. The
//! upstream source of truth lives in `zeroclaw-gateway/src/lib.rs` and the
//! REST reference at `.claude/skills/zeroclaw/references/rest-api.md`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// -------- POST /webhook -------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
pub struct WebhookRequest {
    /// User text. ZeroClaw expects a single `message` field.
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebhookResponse {
    /// Assistant response. Always present on 200 unless the request was a
    /// duplicate idempotency-key hit (`status = "duplicate"`).
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Only present on the idempotency duplicate path.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub idempotent: Option<bool>,
}

// -------- WS /ws/chat ---------------------------------------------------------

/// Outbound frame on `/ws/chat`. The server only recognizes `type = "message"`.
#[derive(Debug, Clone, Serialize)]
pub struct WsClientMessage<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub content: &'a str,
}

impl<'a> WsClientMessage<'a> {
    pub fn new(content: &'a str) -> Self {
        Self {
            kind: "message",
            content,
        }
    }
}

/// Inbound frame on `/ws/chat`. ZeroClaw 0.8 emits `done` (whole assistant
/// reply at once) or `error`. Unknown variants fall through to `Other`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum WsServerMessage {
    /// Turn complete — full assistant reply.
    #[serde(rename = "done")]
    Done {
        #[serde(default)]
        full_response: String,
    },

    /// Future-proofing: if ZeroClaw later emits per-token deltas (currently
    /// not the case), they'll deserialize here and flow into the same
    /// streaming-buffer code path the IronClaw client uses.
    #[serde(rename = "delta")]
    Delta {
        #[serde(default)]
        content: String,
    },

    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        message: String,
    },

    #[serde(other)]
    Other,
}

// -------- GET /api/events (SSE) -----------------------------------------------

/// Event payload on the gateway-wide `/api/events` stream. ZeroClaw 0.8
/// emits these types; we keep the variants we route on and let everything
/// else fall through to `Other` so new server events never crash the client.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum SystemEvent {
    #[serde(rename = "agent_start")]
    AgentStart {
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
    },

    #[serde(rename = "agent_end")]
    AgentEnd {
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        duration_ms: Option<u64>,
        #[serde(default)]
        tokens_used: Option<u64>,
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        timestamp: Option<String>,
    },

    #[serde(rename = "llm_request")]
    LlmRequest {
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
    },

    #[serde(rename = "tool_call_start")]
    ToolCallStart {
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
    },

    #[serde(rename = "tool_call")]
    ToolCall {
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        duration_ms: Option<u64>,
        #[serde(default)]
        success: Option<bool>,
        #[serde(default)]
        timestamp: Option<String>,
    },

    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        component: Option<String>,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
    },

    #[serde(other)]
    Other,
}

/// Parse a raw SSE `data:` payload into a [`SystemEvent`].
///
/// ZeroClaw 0.8 fans BOTH structured observability events (`type = "agent_start"`
/// etc.) AND raw `zeroclaw_log` records onto the same SSE channel — the log
/// records do NOT carry a `type` field, so we first peek the payload and
/// silently collapse type-less or unfamiliar shapes into [`SystemEvent::Other`]
/// instead of bubbling a `missing field "type"` error up the chain (which
/// otherwise floods stderr at every log line emitted by the daemon).
///
/// Returns the raw JSON on a *real* parse failure (malformed JSON) so callers
/// can log unknown shapes.
pub fn parse_system_event(data: &str) -> Result<SystemEvent, (serde_json::Error, Option<Value>)> {
    let value: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(e) => return Err((e, None)),
    };

    // No `type` field, or non-string `type` → this is a log record (or some
    // other un-tagged broadcast frame). Skip silently.
    let Some(_) = value.get("type").and_then(Value::as_str) else {
        return Ok(SystemEvent::Other);
    };

    // Re-deserialize from the in-memory Value. Any unknown `type` value still
    // falls through to `SystemEvent::Other` via the `#[serde(other)]` arm.
    match serde_json::from_value::<SystemEvent>(value.clone()) {
        Ok(ev) => Ok(ev),
        Err(e) => Err((e, Some(value))),
    }
}

// -------- GET /api/sessions / /api/sessions/{id}/messages --------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SessionListResponse {
    #[serde(default)]
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    /// `<uuid>` for gateway sessions (the `gw_` prefix stripped by the
    /// server) or the full composite for channel-driven sessions.
    pub session_id: String,
    /// Full DB key (e.g. `gw_<uuid>`). What we pass back to delete/rename.
    #[serde(default)]
    pub session_key: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub last_activity: Option<String>,
    #[serde(default)]
    pub message_count: Option<usize>,
    #[serde(default)]
    pub agent_alias: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionMessagesResponse {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
    #[serde(default)]
    pub session_persistence: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

// -------- POST/GET/DELETE /api/memory ----------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MemoryWriteRequest {
    pub key: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryEntry {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MemoryListResponse {
    #[serde(default)]
    pub entries: Vec<MemoryEntry>,
}

// -------- GET /api/status / /health ------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StatusResponse {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub gateway_port: Option<u16>,
    #[serde(default)]
    pub uptime_seconds: Option<u64>,
    #[serde(default)]
    pub paired: Option<bool>,
    /// `{ channel_name: enabled_bool }` — populated by ZeroClaw's channel
    /// registry (e.g. `webhook.default`, `voice-duplex.default`).
    #[serde(default)]
    pub channels: std::collections::HashMap<String, bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_response_basic() {
        let r: WebhookResponse =
            serde_json::from_str(r#"{"response":"hello","model":"x"}"#).unwrap();
        assert_eq!(r.response.as_deref(), Some("hello"));
        assert_eq!(r.model.as_deref(), Some("x"));
    }

    #[test]
    fn ws_done_message_parses() {
        let m: WsServerMessage =
            serde_json::from_str(r#"{"type":"done","full_response":"hi"}"#).unwrap();
        match m {
            WsServerMessage::Done { full_response } => assert_eq!(full_response, "hi"),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn ws_unknown_message_is_other() {
        let m: WsServerMessage = serde_json::from_str(r#"{"type":"future_event"}"#).unwrap();
        assert!(matches!(m, WsServerMessage::Other));
    }

    #[test]
    fn system_event_agent_start() {
        let e = parse_system_event(r#"{"type":"agent_start","provider":"openrouter"}"#).unwrap();
        assert!(matches!(e, SystemEvent::AgentStart { .. }));
    }

    #[test]
    fn system_event_missing_type_is_other_silently() {
        // ZeroClaw fans `zeroclaw_log` records onto /api/events; those have no
        // `type` field. Verify we map them to Other instead of erroring out.
        let log_line = r#"{"zc_name":"x","zc_action":"note","level":"info","message":"hi"}"#;
        let ev = parse_system_event(log_line).expect("type-less log should collapse to Other");
        assert!(matches!(ev, SystemEvent::Other));
    }

    #[test]
    fn system_event_unknown_type_is_other() {
        let ev = parse_system_event(r#"{"type":"future_event","x":1}"#).expect("unknown type Ok");
        assert!(matches!(ev, SystemEvent::Other));
    }

    #[test]
    fn system_event_malformed_json_returns_error() {
        let r = parse_system_event("{not json");
        assert!(r.is_err(), "malformed JSON should still surface as an error");
    }

    #[test]
    fn system_event_tool_call_round_trip() {
        let e = parse_system_event(
            r#"{"type":"tool_call","tool":"shell","duration_ms":12,"success":true}"#,
        )
        .unwrap();
        match e {
            SystemEvent::ToolCall {
                tool,
                duration_ms,
                success,
                ..
            } => {
                assert_eq!(tool.as_deref(), Some("shell"));
                assert_eq!(duration_ms, Some(12));
                assert_eq!(success, Some(true));
            }
            _ => panic!("expected ToolCall"),
        }
    }
}
