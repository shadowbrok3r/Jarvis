//! ZeroClaw gateway client surface.
//!
//! Mirrors the layout of [`crate::ironclaw`]:
//! * [`types`] — DTOs for `/webhook`, `/ws/chat`, `/api/events`, `/api/memory`, `/api/status`.
//! * [`client`] — async HTTP + WS + SSE client built on `reqwest`,
//!   `tokio-tungstenite`, and `reqwest-eventsource`. Used by the
//!   `zeroclaw_chat` and `zeroclaw_context` Bevy plugins in the binary crate.
//! * [`events`] — normalized `AppEvent` enum the chat plugin republishes onto
//!   the same Bevy messages the IronClaw path uses, so the chat UI doesn't
//!   need to fork.

pub mod client;
pub mod events;
pub mod types;
