//! Normalized chat-event enum, shaped to match `crate::ironclaw::types::AppEvent`
//! so the Bevy chat UI / TTS / expression plugins can consume both backends
//! without conditional code paths.
//!
//! The translation happens in the `zeroclaw_chat` plugin: raw
//! [`super::types::WsServerMessage`] frames and [`super::types::SystemEvent`]
//! SSE events both get folded into [`AppEvent`] before being published to
//! Bevy.

/// Source of an event — useful for the debug UI to render a small chip
/// indicating "this came from /api/events vs. /ws/chat".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum EventSource {
    /// `/ws/chat` reply (or `/webhook` synchronous reply rewrapped).
    Chat,
    /// `/api/events` server-sent event.
    SystemSse,
}

/// Normalized chat-pipeline event. Variants intentionally mirror the IronClaw
/// `AppEvent` enum so the Bevy chat consumer can reuse the same message types.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum AppEvent {
    /// Final assistant reply — drives `ChatCompleteMessage` + TTS, same as
    /// IronClaw `AppEvent::Response`.
    Response { content: String, model: Option<String> },

    /// Per-token delta. ZeroClaw 0.8 doesn't emit these on the chat surface
    /// today, but the variant exists so a future upstream change drops
    /// straight into the existing streaming-buffer path.
    StreamChunk { content: String },

    /// Surfaced from `/api/events` `llm_request` — useful as a "thinking..."
    /// indicator while the agent loop runs.
    Thinking { message: String },

    /// Tool execution began — from `/api/events` `tool_call_start`.
    ToolStarted { name: String, detail: Option<String> },

    /// Tool execution finished — from `/api/events` `tool_call`.
    ToolCompleted {
        name: String,
        success: bool,
        duration_ms: Option<u64>,
        error: Option<String>,
    },

    /// Free-form status line. Used for connection state + `agent_start` /
    /// `agent_end` summaries.
    Status { message: String },

    /// Hard error: WS close, SSE auth rejection, webhook 5xx.
    Error { message: String },
}

impl AppEvent {
    /// Short label for the debug UI / traffic log.
    pub fn label(&self) -> &'static str {
        match self {
            AppEvent::Response { .. } => "response",
            AppEvent::StreamChunk { .. } => "stream_chunk",
            AppEvent::Thinking { .. } => "thinking",
            AppEvent::ToolStarted { .. } => "tool_started",
            AppEvent::ToolCompleted { .. } => "tool_completed",
            AppEvent::Status { .. } => "status",
            AppEvent::Error { .. } => "error",
        }
    }
}
