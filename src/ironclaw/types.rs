//! DTOs for the IronClaw gateway (`:3000`) HTTP + SSE API.
//!
//! Kept intentionally thin — only the fields the Rust client reads/writes. We
//! quote every UUID as a plain `String` (wire format is the hyphenated UUID
//! text); the avatar never does UUID arithmetic on them.
//!
//! Source of truth lives in:
//! * `ironclaw-staging/src/channels/web/types.rs`
//! * `ironclaw-staging/crates/ironclaw_common/src/event.rs`

use serde::{Deserialize, Serialize};
use serde_json::Value;

// -------- POST /api/chat/send -------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
pub struct SendMessageRequest {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageData>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageResponse {
    pub message_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageData {
    /// e.g. `image/png`, `image/jpeg`.
    pub media_type: String,
    /// Base64 payload, no `data:` prefix.
    pub data: String,
}

// -------- Threads -------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ThreadInfo {
    pub id: String,
    pub state: String,
    pub turn_count: usize,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub thread_type: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreadListResponse {
    #[serde(default)]
    pub assistant_thread: Option<ThreadInfo>,
    #[serde(default)]
    pub threads: Vec<ThreadInfo>,
    #[serde(default)]
    pub active_thread: Option<String>,
}

// -------- History -------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TurnInfo {
    pub turn_number: usize,
    #[serde(default)]
    pub user_message_id: Option<String>,
    pub user_input: String,
    #[serde(default)]
    pub response: Option<String>,
    pub state: String,
    pub started_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallInfo>,
    #[serde(default)]
    pub narrative: Option<String>,
    /// Model reasoning / chain-of-thought text when the gateway records it on the turn.
    #[serde(default, alias = "reasoning")]
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallInfo {
    pub name: String,
    #[serde(default)]
    pub has_result: bool,
    #[serde(default)]
    pub has_error: bool,
    #[serde(default)]
    pub result_preview: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HistoryResponse {
    pub thread_id: String,
    #[serde(default)]
    pub turns: Vec<TurnInfo>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub oldest_timestamp: Option<String>,
}

// -------- SSE AppEvent --------------------------------------------------------
//
// On the wire the gateway sends `{"type":"<tag>", …fields}`. We mirror only the
// variants the avatar acts on; everything else falls through to `Other` so new
// server events never crash the client.

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum AppEvent {
    /// Turn complete — drives `ChatCompleteMessage` + TTS.
    #[serde(rename = "response")]
    Response { content: String, thread_id: String },

    /// "Thinking…" indicator in the debug UI.
    #[serde(rename = "thinking")]
    Thinking {
        message: String,
        #[serde(default)]
        thread_id: Option<String>,
    },

    /// Per-token assistant delta (live streaming).
    #[serde(rename = "stream_chunk")]
    StreamChunk {
        content: String,
        #[serde(default)]
        thread_id: Option<String>,
    },

    /// Generic status line (gateway progress messages).
    #[serde(rename = "status")]
    Status {
        message: String,
        #[serde(default)]
        thread_id: Option<String>,
    },

    #[serde(rename = "tool_started")]
    ToolStarted {
        name: String,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        thread_id: Option<String>,
    },

    #[serde(rename = "tool_completed")]
    ToolCompleted {
        name: String,
        success: bool,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        thread_id: Option<String>,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        name: String,
        preview: String,
        #[serde(default)]
        thread_id: Option<String>,
    },

    /// Emitted when a tool (e.g. image MCP) produced an image — matches IronClaw `AppEvent::ImageGenerated`.
    #[serde(rename = "image_generated")]
    ImageGenerated {
        event_id: String,
        data_url: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        thread_id: Option<String>,
    },

    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(default)]
        thread_id: Option<String>,
    },

    /// Catch-all for event variants we don't actively route. Retained so logs
    /// and the debug UI can surface them without us needing to enumerate every
    /// IronClaw event type.
    #[serde(other)]
    Other,
}

impl AppEvent {
    /// Lightweight `thread_id` accessor used for routing filtering.
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            AppEvent::Response { thread_id, .. } => Some(thread_id.as_str()),
            AppEvent::Thinking { thread_id, .. }
            | AppEvent::StreamChunk { thread_id, .. }
            | AppEvent::Status { thread_id, .. }
            | AppEvent::ToolStarted { thread_id, .. }
            | AppEvent::ToolCompleted { thread_id, .. }
            | AppEvent::ToolResult { thread_id, .. }
            | AppEvent::ImageGenerated { thread_id, .. }
            | AppEvent::Error { thread_id, .. } => thread_id.as_deref(),
            AppEvent::Other => None,
        }
    }
}

/// Parse raw SSE `data:` payload into an [`AppEvent`]. On deserialization
/// failure we return the raw JSON so callers can log/debug unknown shapes.
pub fn parse_app_event(data: &str) -> Result<AppEvent, (serde_json::Error, Option<Value>)> {
    match serde_json::from_str::<AppEvent>(data) {
        Ok(ev) => Ok(ev),
        Err(e) => {
            let fallback = serde_json::from_str::<Value>(data).ok();
            Err((e, fallback))
        }
    }
}
