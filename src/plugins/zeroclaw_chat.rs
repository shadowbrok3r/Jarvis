//! ZeroClaw gateway chat client.
//!
//! Parallel to [`super::ironclaw_chat`] — selects automatically when
//! `gateway.backend = "zeroclaw"`. Activates a single dedicated tokio thread
//! that drives two flows:
//!
//! 1. **Chat round-trip** — outbound user text via either `/ws/chat`
//!    (default; ZeroClaw 0.8 returns a single `done` frame with the full
//!    assistant reply) or `POST /webhook`. The client prepends a rolling
//!    window of recent turns so the (stateless) gateway still has context.
//! 2. **System event stream** — long-lived SSE subscription to
//!    `/api/events`. Tool / agent activity is republished as
//!    [`ToolEventMessage`] and the `agent_start`/`agent_end` boundary drives
//!    [`ThinkingStateMessage`] so the chat UI gets the same "Thinking…"
//!    indicator the IronClaw path has.
//!
//! Reuses [`ChatState`], [`GatewayClientHandle`], and every chat
//! [`Message`] type defined in [`super::ironclaw_chat`] so the existing chat
//! UI and TTS / expression plugins are entirely backend-agnostic.

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded};
use futures_util::{SinkExt, StreamExt};
use reqwest_eventsource::Event as SseEvent;
use serde_json::{Value, json};
use tokio::runtime::Builder;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::act::{should_skip_tts_for_error_like_response, strip_act_delay_for_tts};
use crate::config::{ChatBackend, Settings, ZeroClawSettings};
use crate::ironclaw::types::{ImageData, ThreadInfo};
use crate::zeroclaw::client::{ClientIdentity, ZeroClawClient, is_auth_failure};
use crate::zeroclaw::types::{
    SystemEvent, WsClientMessage, WsServerMessage, parse_system_event,
};

use super::channel_server::{ChatCompleteMessage, HubBroadcast, TtsSpeakMessage};
use super::chat_pipeline_status::{ChatPipelineStage, ChatPipelineStatus};
use super::ironclaw_chat::{
    AssistantDeltaMessage, ChatCommand, ChatState, ChatStatusMessage, GatewayClientHandle,
    HistoryMessage, ThinkingStateMessage, ThreadListMessage, ToolEventMessage, ToolPhase,
    TranscriptLine, TranscriptRole,
};
use super::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};

/// Fallback "thread" id used while the worker is still bootstrapping a
/// real session from the gateway. Overwritten by the persisted
/// `[zeroclaw].active_session_id` (or a freshly minted uuid) as soon as
/// the boot routine runs.
const FALLBACK_THREAD_ID: &str = "zeroclaw-default";


pub struct ZeroClawChatPlugin;

impl Plugin for ZeroClawChatPlugin {
    fn build(&self, app: &mut App) {
        // We deliberately do NOT init `ChatState` here — `IronclawChatPlugin`
        // already does it. Same for chat-related Messages. Both backends
        // re-use the same Bevy facing types so the chat UI stays single-path.
        app.add_systems(PostStartup, spawn_zeroclaw_thread)
            .add_systems(Update, pump_zeroclaw_into_bevy);
    }
}

// ---------- Worker → Bevy events ----------------------------------------------

#[derive(Resource)]
struct ZeroClawInbound {
    rx: Receiver<InboundEvent>,
}

#[derive(Debug, Clone)]
enum InboundEvent {
    /// Final assistant reply.
    Response { content: String, model: Option<String> },
    /// Per-token delta (future-proof; not emitted by ZeroClaw 0.8 chat).
    StreamDelta(String),
    /// Tool / agent activity surfaced from `/api/events`.
    SystemEvent(SystemEvent),
    /// Transport-level state change.
    Status { status: Option<String>, error: Option<String> },
    /// Thread list / active thread changes (mostly synthetic for ZeroClaw).
    Threads(Vec<ThreadInfo>),
    ActiveThread(Option<String>),
    /// History payload — emitted on `LoadHistory` from the local rolling buffer.
    History {
        thread_id: String,
        turns: Vec<TranscriptLine>,
    },
}

// ---------- Bootstrap ---------------------------------------------------------

fn spawn_zeroclaw_thread(
    mut commands: Commands,
    settings: Res<Settings>,
    hub: Res<HubBroadcast>,
    traffic: Option<Res<TrafficLogSink>>,
) {
    if !matches!(
        ChatBackend::parse(&settings.gateway.backend),
        ChatBackend::Zeroclaw,
    ) {
        debug!("zeroclaw_chat: gateway.backend = {:?}; not starting", settings.gateway.backend);
        return;
    }
    let cfg = settings.zeroclaw.clone();
    let module_name = settings.ironclaw.module_name.clone();
    let hub_tx = hub.clone();
    let traffic = traffic.map(|t| (*t).clone());

    let (cmd_tx, cmd_rx) = unbounded::<ChatCommand>();
    let (in_tx, in_rx) = unbounded::<InboundEvent>();

    commands.insert_resource(GatewayClientHandle::__new_for_backend(cmd_tx));
    commands.insert_resource(ZeroClawInbound { rx: in_rx });
    commands.insert_resource(ChatState {
        base_url: cfg.normalized_base_url(),
        has_bearer: !cfg.auth_token.is_empty(),
        ..Default::default()
    });

    info!(
        "zeroclaw_chat: starting worker (base={}, ws={}, attachments={}, prefer_streaming={})",
        cfg.normalized_base_url(),
        cfg.resolved_ws_url(),
        cfg.attachments_enabled,
        cfg.prefer_streaming
    );

    thread::Builder::new()
        .name("jarvis-zeroclaw".into())
        .spawn(move || {
            let rt = match Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("zeroclaw tokio runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(run_worker(cfg, module_name, cmd_rx, in_tx, hub_tx, traffic));
        })
        .expect("failed to spawn jarvis-zeroclaw thread");
}

// ---------- Worker -----------------------------------------------------------

async fn run_worker(
    cfg: ZeroClawSettings,
    module_name: String,
    cmd_rx: Receiver<ChatCommand>,
    in_tx: Sender<InboundEvent>,
    hub: HubBroadcast,
    traffic: Option<TrafficLogSink>,
) {
    let identity = ClientIdentity {
        client_id: cfg.client_id.clone(),
        webhook_secret: cfg.webhook_secret.clone(),
    };
    let client = Arc::new(ZeroClawClient::new(
        cfg.normalized_base_url(),
        cfg.resolved_ws_url(),
        cfg.auth_token.clone(),
        identity,
        cfg.agent_alias.clone(),
        cfg.request_timeout_ms,
    ));

    // Resolve the active session id: prefer the persisted value, otherwise
    // mint a fresh uuid and persist it on first real send.
    let mut active_session: String = cfg.active_session_id.trim().to_string();
    if active_session.is_empty() {
        active_session = uuid::Uuid::new_v4().to_string();
    }
    let mut session_persisted = !cfg.active_session_id.trim().is_empty();

    // Pull the real session list from `/api/sessions` filtered to our agent;
    // fall back to a single placeholder row when the gateway has nothing yet.
    let session_limit = cfg.session_list_limit as usize;
    refresh_session_list(
        &client,
        &cfg.agent_alias,
        &active_session,
        session_limit,
        &in_tx,
    )
    .await;
    let _ = in_tx.send(InboundEvent::ActiveThread(Some(active_session.clone())));

    // Auto-load the persisted transcript for the resumed session so users
    // see prior messages without having to click their own thread first.
    if session_persisted {
        load_session_history(&client, &active_session, &in_tx).await;
    }

    // Initial health probe so the chat UI shows "online" when reachable.
    match client.health().await {
        Ok(true) => {
            let _ = in_tx.send(InboundEvent::Status {
                status: Some("connected".into()),
                error: None,
            });
        }
        Ok(false) => {
            let _ = in_tx.send(InboundEvent::Status {
                status: None,
                error: Some("zeroclaw /health returned non-2xx".into()),
            });
        }
        Err(e) => {
            let _ = in_tx.send(InboundEvent::Status {
                status: None,
                error: Some(format!("zeroclaw /health: {e}")),
            });
        }
    }

    // Start the system-event SSE listener task (long-lived, retries internally).
    {
        let client = Arc::clone(&client);
        let in_tx = in_tx.clone();
        let traffic_es = traffic.clone();
        tokio::spawn(async move {
            run_event_stream(client, in_tx, traffic_es).await;
        });
    }

    // Rolling history used to prepend prior turns to the next outbound
    // message — ZeroClaw chat is stateless on the wire.
    let mut history: VecDeque<(TranscriptRole, String)> = VecDeque::new();
    let history_window = cfg.history_window.max(0) as usize;

    loop {
        let cmd = match cmd_rx.try_recv() {
            Ok(c) => c,
            Err(TryRecvError::Empty) => {
                tokio::time::sleep(Duration::from_millis(30)).await;
                continue;
            }
            Err(TryRecvError::Disconnected) => return,
        };

        match cmd {
            ChatCommand::Send { text, images, .. } => {
                handle_send(
                    &client,
                    &cfg,
                    &module_name,
                    &hub,
                    traffic.as_ref(),
                    &in_tx,
                    &mut history,
                    history_window,
                    &active_session,
                    text,
                    images,
                )
                .await;
                // First successful send creates the session server-side;
                // persist the id so a restart resumes the same transcript.
                if !session_persisted {
                    if let Err(e) = persist_active_session(&active_session) {
                        warn!("zeroclaw_chat: failed to persist session id: {e}");
                    }
                    session_persisted = true;
                }
                // Refresh the sidebar so the new session shows up with
                // updated message count + last activity.
                refresh_session_list(
                    &client,
                    &cfg.agent_alias,
                    &active_session,
                    session_limit,
                    &in_tx,
                )
                .await;
            }
            ChatCommand::RefreshThreads => {
                refresh_session_list(
                    &client,
                    &cfg.agent_alias,
                    &active_session,
                    session_limit,
                    &in_tx,
                )
                .await;
            }
            ChatCommand::NewThread => {
                // Mint a fresh session id; the new one becomes active and
                // existing sessions stay on the gateway (user can switch
                // back via the sidebar). Persisted on first send.
                history.clear();
                active_session = uuid::Uuid::new_v4().to_string();
                session_persisted = false;
                let _ = in_tx.send(InboundEvent::ActiveThread(Some(active_session.clone())));
                let _ = in_tx.send(InboundEvent::History {
                    thread_id: active_session.clone(),
                    turns: Vec::new(),
                });
                refresh_session_list(
                    &client,
                    &cfg.agent_alias,
                    &active_session,
                    session_limit,
                    &in_tx,
                )
                .await;
                let _ = in_tx.send(InboundEvent::Status {
                    status: Some(format!("new session {}", short_session(&active_session))),
                    error: None,
                });
            }
            ChatCommand::SetActiveThread(id) => {
                active_session = id.clone();
                session_persisted = id != FALLBACK_THREAD_ID;
                if session_persisted {
                    if let Err(e) = persist_active_session(&active_session) {
                        warn!("zeroclaw_chat: failed to persist session id: {e}");
                    }
                }
                history.clear();
                let _ = in_tx.send(InboundEvent::ActiveThread(Some(active_session.clone())));
                load_session_history(&client, &active_session, &in_tx).await;
            }
            ChatCommand::LoadHistory { thread_id, .. } => {
                load_session_history(&client, &thread_id, &in_tx).await;
            }
        }
    }
}

fn short_session(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Persist `active_session_id` back to `config/user.toml` so the next launch
/// resumes the same gateway session.
fn persist_active_session(session_id: &str) -> Result<(), String> {
    let mut settings = crate::config::Settings::load().map_err(|e| e.to_string())?;
    if settings.zeroclaw.active_session_id == session_id {
        return Ok(());
    }
    settings.zeroclaw.active_session_id = session_id.to_string();
    settings.save_user()
}

async fn refresh_session_list(
    client: &ZeroClawClient,
    agent_alias: &str,
    active: &str,
    limit: usize,
    in_tx: &Sender<InboundEvent>,
) {
    let list = match client.list_sessions().await {
        Ok(l) => l,
        Err(e) => {
            warn!("zeroclaw_chat: list_sessions: {e}");
            // Still emit a single fallback entry so the sidebar isn't blank.
            let placeholder = ThreadInfo {
                id: active.to_string(),
                state: "active".into(),
                turn_count: 0,
                created_at: String::new(),
                updated_at: String::new(),
                title: Some(format!("session {}", short_session(active))),
                thread_type: Some("zeroclaw".into()),
                channel: Some("webhook.default".into()),
            };
            let _ = in_tx.send(InboundEvent::Threads(vec![placeholder]));
            return;
        }
    };

    // Surface sessions belonging to the configured agent (plus any anonymous
    // ones from older runs), sorted by `last_activity` desc so the most
    // recent shows up first. Inject the active session even if the server
    // hasn't persisted it yet (first send of a new chat).
    let mut sessions: Vec<&crate::zeroclaw::types::SessionInfo> = list
        .sessions
        .iter()
        .filter(|s| match s.agent_alias.as_deref() {
            Some(a) => a == agent_alias,
            None => true,
        })
        .collect();
    sessions.sort_by(|a, b| {
        b.last_activity
            .as_deref()
            .cmp(&a.last_activity.as_deref())
    });

    let mut threads: Vec<ThreadInfo> = sessions
        .iter()
        .take(limit)
        .map(|s| ThreadInfo {
            id: s.session_id.clone(),
            state: "active".into(),
            turn_count: s.message_count.unwrap_or(0),
            created_at: s.created_at.clone().unwrap_or_default(),
            updated_at: s.last_activity.clone().unwrap_or_default(),
            title: Some(
                s.name
                    .clone()
                    .unwrap_or_else(|| format!("session {}", short_session(&s.session_id))),
            ),
            thread_type: Some("zeroclaw".into()),
            channel: s
                .channel_id
                .clone()
                .or_else(|| Some("webhook.default".into())),
        })
        .collect();
    if !threads.iter().any(|t| t.id == active) {
        threads.insert(
            0,
            ThreadInfo {
                id: active.to_string(),
                state: "active".into(),
                turn_count: 0,
                created_at: String::new(),
                updated_at: String::new(),
                title: Some(format!("session {} (new)", short_session(active))),
                thread_type: Some("zeroclaw".into()),
                channel: Some("webhook.default".into()),
            },
        );
    }
    let _ = in_tx.send(InboundEvent::Threads(threads));
}

async fn load_session_history(
    client: &ZeroClawClient,
    session_id: &str,
    in_tx: &Sender<InboundEvent>,
) {
    let resp = match client.session_messages(session_id).await {
        Ok(r) => r,
        Err(e) => {
            warn!("zeroclaw_chat: session_messages({session_id}): {e}");
            let _ = in_tx.send(InboundEvent::History {
                thread_id: session_id.to_string(),
                turns: Vec::new(),
            });
            return;
        }
    };
    let turns: Vec<TranscriptLine> = resp
        .messages
        .into_iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "assistant" | "ai" => TranscriptRole::Assistant,
                "tool" => TranscriptRole::Tool,
                "system" => TranscriptRole::System,
                _ => TranscriptRole::User,
            };
            TranscriptLine {
                role,
                text: m.content,
                thinking: None,
                suggestions: Vec::new(),
                tool_calls_json: None,
                images: Vec::new(),
            }
        })
        .collect();
    let _ = in_tx.send(InboundEvent::History {
        thread_id: session_id.to_string(),
        turns,
    });
}

fn history_to_transcript(history: &VecDeque<(TranscriptRole, String)>) -> Vec<TranscriptLine> {
    history
        .iter()
        .map(|(role, text)| TranscriptLine {
            role: *role,
            text: text.clone(),
            thinking: None,
            suggestions: Vec::new(),
            tool_calls_json: None,
            images: Vec::new(),
        })
        .collect()
}

// ---------- Send handler -----------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn handle_send(
    client: &Arc<ZeroClawClient>,
    cfg: &ZeroClawSettings,
    module_name: &str,
    hub: &HubBroadcast,
    traffic: Option<&TrafficLogSink>,
    in_tx: &Sender<InboundEvent>,
    history: &mut VecDeque<(TranscriptRole, String)>,
    history_window: usize,
    session_id: &str,
    text: String,
    images: Vec<ImageData>,
) {
    let mut outbound_text = text.trim().to_string();
    if outbound_text.is_empty() && images.is_empty() {
        return;
    }

    // Attachment workaround: ZeroClaw chat surfaces have no binary field, so
    // we route the bytes through our own HTTP server (see
    // `zeroclaw_attachments`) and inline the URLs in the message text.
    if cfg.attachments_enabled && !images.is_empty() {
        if let Some(public_url) = super::zeroclaw_attachments::publish_images(&images) {
            if !outbound_text.is_empty() {
                outbound_text.push_str("\n\n");
            }
            outbound_text.push_str("[Attached images (HTTP fetchable):\n");
            for url in public_url {
                outbound_text.push_str("- ");
                outbound_text.push_str(&url);
                outbound_text.push('\n');
            }
            outbound_text.push(']');
        }
    }

    // Compose the final wire payload with the rolling history block.
    let wire_text = compose_wire_text(history, history_window, &outbound_text);

    // Push the user turn into local history BEFORE the round-trip so a slow
    // reply doesn't leave us re-prepending stale state on retry.
    push_history(history, history_window, TranscriptRole::User, outbound_text.clone());

    if let Some(log) = traffic {
        log.push(
            TrafficChannel::ZeroClawHttp,
            TrafficDirection::Outbound,
            if cfg.prefer_streaming {
                "WS /ws/chat (open + send)"
            } else {
                "POST /webhook"
            },
            Some(json!({
                "preview": outbound_text.chars().take(200).collect::<String>(),
                "history_turns_prepended": history_block_turn_count(history, history_window),
                "attachments": images.len(),
            })),
        );
    }

    let _ = in_tx.send(InboundEvent::Status {
        status: Some(if cfg.prefer_streaming { "ws: sending" } else { "webhook: sending" }.into()),
        error: None,
    });

    let (reply, model) = if cfg.prefer_streaming {
        match send_via_ws(client, &wire_text, session_id, in_tx, traffic).await {
            Ok(reply) => (reply, None),
            Err(e) => {
                let _ = in_tx.send(InboundEvent::Status {
                    status: None,
                    error: Some(format!("ws: {e}")),
                });
                return;
            }
        }
    } else {
        match client.webhook(&wire_text, None).await {
            Ok(resp) => {
                let reply = resp
                    .response
                    .unwrap_or_else(|| {
                        if matches!(resp.status.as_deref(), Some("duplicate")) {
                            "(duplicate request — ZeroClaw returned the previous reply silently)".into()
                        } else {
                            String::new()
                        }
                    });
                (reply, resp.model)
            }
            Err(e) => {
                let _ = in_tx.send(InboundEvent::Status {
                    status: None,
                    error: Some(format!("webhook: {e}")),
                });
                return;
            }
        }
    };

    if reply.is_empty() {
        let _ = in_tx.send(InboundEvent::Status {
            status: Some("(empty reply)".into()),
            error: None,
        });
        return;
    }

    // Republish onto the channel hub so peers (server.mjs voice pipeline,
    // ironclaw-proxy, etc.) get `output:gen-ai:chat:complete` regardless of
    // which chat backend produced the reply.
    publish_chat_complete(hub, module_name, &reply, session_id);

    push_history(history, history_window, TranscriptRole::Assistant, reply.clone());
    let _ = in_tx.send(InboundEvent::Response { content: reply, model });
}

fn compose_wire_text(
    history: &VecDeque<(TranscriptRole, String)>,
    history_window: usize,
    user_text: &str,
) -> String {
    let take_from = history.len().saturating_sub(history_window);
    let slice: Vec<&(TranscriptRole, String)> = history.iter().skip(take_from).collect();
    if slice.is_empty() {
        return user_text.to_string();
    }
    let mut out = String::from("[Conversation so far:\n");
    for (role, text) in slice {
        let label = match role {
            TranscriptRole::User => "User",
            TranscriptRole::Assistant => "Assistant",
            TranscriptRole::Tool => "Tool",
            TranscriptRole::System => "System",
        };
        out.push_str(label);
        out.push_str(": ");
        out.push_str(text);
        out.push('\n');
    }
    out.push_str("]\n\n");
    out.push_str(user_text);
    out
}

fn history_block_turn_count(
    history: &VecDeque<(TranscriptRole, String)>,
    history_window: usize,
) -> usize {
    history.len().min(history_window)
}

fn push_history(
    history: &mut VecDeque<(TranscriptRole, String)>,
    history_window: usize,
    role: TranscriptRole,
    text: String,
) {
    if history_window == 0 {
        return;
    }
    history.push_back((role, text));
    while history.len() > history_window {
        history.pop_front();
    }
}

fn publish_chat_complete(hub: &HubBroadcast, module_name: &str, content: &str, session_id: &str) {
    let envelope = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "from": module_name,
        "thread_id": session_id,
        "message": {
            "role": "assistant",
            "content": content,
        },
    });
    hub.send("output:gen-ai:chat:complete", envelope);
}

// ---------- WS send path ------------------------------------------------------

async fn send_via_ws(
    client: &Arc<ZeroClawClient>,
    wire_text: &str,
    session_id: &str,
    in_tx: &Sender<InboundEvent>,
    traffic: Option<&TrafficLogSink>,
) -> Result<String, String> {
    let mut ws = client
        .open_chat_ws(Some(session_id))
        .await
        .map_err(|e| e.to_string())?;
    let frame = serde_json::to_string(&WsClientMessage::new(wire_text))
        .map_err(|e| format!("serialize: {e}"))?;
    ws.send(WsMessage::Text(frame.clone().into()))
        .await
        .map_err(|e| format!("send: {e}"))?;

    let _ = in_tx.send(InboundEvent::Status {
        status: Some("ws: awaiting reply".into()),
        error: None,
    });

    loop {
        let Some(item) = ws.next().await else {
            return Err("ws closed before reply".into());
        };
        let msg = item.map_err(|e| format!("recv: {e}"))?;
        let text = match msg {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Binary(_) => continue,
            WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => continue,
            WsMessage::Close(_) => return Err("ws closed mid-reply".into()),
        };
        if let Some(log) = traffic {
            log.push(
                TrafficChannel::ZeroClawWs,
                TrafficDirection::Inbound,
                "ws frame",
                serde_json::from_str::<Value>(&text).ok(),
            );
        }
        match serde_json::from_str::<WsServerMessage>(&text) {
            Ok(WsServerMessage::Done { full_response }) => {
                let _ = ws.close(None).await;
                return Ok(full_response);
            }
            Ok(WsServerMessage::Delta { content }) => {
                if !content.is_empty() {
                    let _ = in_tx.send(InboundEvent::StreamDelta(content));
                }
            }
            Ok(WsServerMessage::Error { message }) => {
                let _ = ws.close(None).await;
                return Err(message);
            }
            Ok(WsServerMessage::Other) => continue,
            Err(e) => {
                warn!("zeroclaw ws: parse error: {e}; raw={}", text.chars().take(160).collect::<String>());
            }
        }
    }
}

// ---------- /api/events stream -----------------------------------------------

async fn run_event_stream(
    client: Arc<ZeroClawClient>,
    in_tx: Sender<InboundEvent>,
    traffic: Option<TrafficLogSink>,
) {
    let mut backoff_ms: u64 = 1_000;
    loop {
        let mut es = client.open_event_stream();
        let _ = in_tx.send(InboundEvent::Status {
            status: Some("sse: connecting".into()),
            error: None,
        });
        while let Some(item) = es.next().await {
            match item {
                Ok(SseEvent::Open) => {
                    backoff_ms = 1_000;
                    let _ = in_tx.send(InboundEvent::Status {
                        status: Some("sse: open".into()),
                        error: None,
                    });
                }
                Ok(SseEvent::Message(msg)) => {
                    if let Some(ref log) = traffic {
                        log.push(
                            TrafficChannel::ZeroClawSse,
                            TrafficDirection::Inbound,
                            format!(
                                "sse id={}",
                                msg.id.chars().take(32).collect::<String>()
                            ),
                            serde_json::from_str::<Value>(&msg.data).ok(),
                        );
                    }
                    match parse_system_event(&msg.data) {
                        Ok(ev) => {
                            let _ = in_tx.send(InboundEvent::SystemEvent(ev));
                        }
                        Err((e, fallback)) => {
                            // After the type-peek change in
                            // `zeroclaw::types::parse_system_event`, the
                            // only payloads that reach this branch are
                            // truly malformed JSON (network corruption,
                            // partial frames, etc.). Untyped log records
                            // collapse into `SystemEvent::Other` silently.
                            debug!(
                                "zeroclaw sse: skipping malformed frame: {e}; raw type={:?}",
                                fallback.as_ref().and_then(|v| v.get("type"))
                            );
                        }
                    }
                }
                Err(e) => {
                    let auth_failed = matches!(
                        &e,
                        reqwest_eventsource::Error::InvalidStatusCode(code, _)
                            if code.as_u16() == 401 || code.as_u16() == 403
                    );
                    es.close();
                    let _ = in_tx.send(InboundEvent::Status {
                        status: None,
                        error: Some(format!("sse: {e}")),
                    });
                    if auth_failed {
                        warn!("zeroclaw sse auth rejected; stopping reconnect loop");
                        // Map through the auth-failure helper so callers can
                        // distinguish if we ever switch to a typed error.
                        let _ = is_auth_failure(&crate::zeroclaw::client::ZeroClawError::Status {
                            code: reqwest::StatusCode::UNAUTHORIZED,
                            body: String::new(),
                        });
                        return;
                    }
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = backoff_ms.saturating_mul(2).min(30_000);
    }
}

// ---------- Bevy pump (worker → Bevy messages) ------------------------------

#[allow(clippy::too_many_arguments)]
fn pump_zeroclaw_into_bevy(
    inbound: Option<Res<ZeroClawInbound>>,
    mut delta: MessageWriter<AssistantDeltaMessage>,
    mut thinking: MessageWriter<ThinkingStateMessage>,
    mut tools: MessageWriter<ToolEventMessage>,
    mut threads: MessageWriter<ThreadListMessage>,
    mut history: MessageWriter<HistoryMessage>,
    mut status: MessageWriter<ChatStatusMessage>,
    mut chat_complete: MessageWriter<ChatCompleteMessage>,
    mut tts: MessageWriter<TtsSpeakMessage>,
    mut state: ResMut<ChatState>,
    mut pipeline: ResMut<ChatPipelineStatus>,
) {
    let Some(inbound) = inbound else { return };
    while let Ok(ev) = inbound.rx.try_recv() {
        match ev {
            InboundEvent::Response { content, model } => {
                let speak = strip_act_delay_for_tts(&content).to_string();
                chat_complete.write(ChatCompleteMessage {
                    content: content.clone(),
                });
                if should_skip_tts_for_error_like_response(&speak) {
                    pipeline.set(
                        ChatPipelineStage::Idle,
                        "zeroclaw reply (error-like; TTS skipped)",
                    );
                } else if !speak.trim().is_empty() {
                    pipeline.set(
                        ChatPipelineStage::KokoroQueued,
                        format!("TTS {} chars", speak.len()),
                    );
                    tts.write(TtsSpeakMessage { text: speak });
                } else {
                    pipeline.set(ChatPipelineStage::Idle, "zeroclaw reply (empty after ACT strip)");
                }
                thinking.write(ThinkingStateMessage {
                    active: false,
                    text: String::new(),
                });
                if let Some(model) = model {
                    state.last_status = Some(format!("reply via {model}"));
                }
            }
            InboundEvent::StreamDelta(content) => {
                pipeline.set(
                    ChatPipelineStage::AiStreaming,
                    format!("delta +{} chars", content.len()),
                );
                delta.write(AssistantDeltaMessage {
                    // ZeroClaw 0.8 doesn't actually emit `delta` frames on
                    // `/ws/chat` today; if it ever starts, the chat UI
                    // doesn't filter by thread_id for streaming text so a
                    // placeholder here is fine. We use the fallback id
                    // since the system-event pipe doesn't carry session
                    // context.
                    thread_id: Some(FALLBACK_THREAD_ID.to_string()),
                    delta: content,
                });
            }
            InboundEvent::SystemEvent(ev) => handle_system_event(
                ev,
                &mut thinking,
                &mut tools,
                &mut pipeline,
            ),
            InboundEvent::Status { status: s, error } => {
                if let Some(ref msg) = error {
                    state.last_error = Some(msg.clone());
                } else {
                    state.last_error = None;
                }
                if let Some(ref msg) = s {
                    state.last_status = Some(msg.clone());
                }
                status.write(ChatStatusMessage { status: s, error });
            }
            InboundEvent::Threads(list) => {
                state.threads = list.clone();
                threads.write(ThreadListMessage(list));
            }
            InboundEvent::ActiveThread(id) => {
                state.active_thread = id;
            }
            InboundEvent::History { thread_id, turns } => {
                state.transcript.clear();
                for line in turns.iter().cloned() {
                    state.transcript.push_back(line);
                }
                history.write(HistoryMessage { thread_id, turns });
            }
        }
    }
}

fn handle_system_event(
    ev: SystemEvent,
    thinking: &mut MessageWriter<ThinkingStateMessage>,
    tools: &mut MessageWriter<ToolEventMessage>,
    pipeline: &mut ChatPipelineStatus,
) {
    match ev {
        SystemEvent::AgentStart { provider, model, .. } => {
            let detail = format!(
                "agent {}/{}",
                provider.as_deref().unwrap_or("?"),
                model.as_deref().unwrap_or("?")
            );
            pipeline.set(ChatPipelineStage::AiThinking, detail.clone());
            thinking.write(ThinkingStateMessage {
                active: true,
                text: detail,
            });
        }
        SystemEvent::AgentEnd {
            duration_ms,
            tokens_used,
            cost_usd,
            ..
        } => {
            thinking.write(ThinkingStateMessage {
                active: false,
                text: String::new(),
            });
            tools.write(ToolEventMessage {
                phase: ToolPhase::Result,
                tool: "agent".into(),
                payload: json!({
                    "duration_ms": duration_ms,
                    "tokens_used": tokens_used,
                    "cost_usd": cost_usd,
                }),
            });
        }
        SystemEvent::LlmRequest { provider, model, .. } => {
            tools.write(ToolEventMessage {
                phase: ToolPhase::Started,
                tool: "llm".into(),
                payload: json!({ "provider": provider, "model": model }),
            });
        }
        SystemEvent::ToolCallStart { tool, .. } => {
            let name = tool.unwrap_or_else(|| "tool".into());
            pipeline.set(ChatPipelineStage::ToolRunning, name.clone());
            tools.write(ToolEventMessage {
                phase: ToolPhase::Started,
                tool: name,
                payload: json!({}),
            });
        }
        SystemEvent::ToolCall {
            tool,
            duration_ms,
            success,
            ..
        } => {
            let name = tool.unwrap_or_else(|| "tool".into());
            let ok = success.unwrap_or(true);
            pipeline.set(
                ChatPipelineStage::Idle,
                format!("tool {name} ok={ok} {}ms", duration_ms.unwrap_or(0)),
            );
            tools.write(ToolEventMessage {
                phase: ToolPhase::Completed,
                tool: name,
                payload: json!({ "success": ok, "duration_ms": duration_ms }),
            });
        }
        SystemEvent::Error { component, message, .. } => {
            tools.write(ToolEventMessage {
                phase: ToolPhase::Result,
                tool: component.unwrap_or_else(|| "error".into()),
                payload: json!({ "message": message }),
            });
            thinking.write(ThinkingStateMessage {
                active: false,
                text: String::new(),
            });
        }
        SystemEvent::Other => {}
    }
}
