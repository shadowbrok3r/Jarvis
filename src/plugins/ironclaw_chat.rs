//! IronClaw gateway chat client.
//!
//! Owns a dedicated tokio thread that talks to the gateway at
//! `settings.gateway.base_url`. Two flows:
//!
//! 1. **Outbound** — `ChatCommand`s (send message, switch thread, refresh
//!    thread list, history) come from the debug UI or from the channel hub
//!    (`HubInputTextMessage` from `server.mjs`'s voice pipeline).
//! 2. **Inbound** — a long-lived `EventSource` listens on `/api/chat/events`
//!    and re-publishes each [`AppEvent`] back into Bevy as message types the
//!    UI / expressions / TTS plugins consume.
//!
//! On `AppEvent::Response` we also republish `output:gen-ai:chat:complete`
//! onto the channel hub so `server.mjs` keeps receiving it for `haAnnounce`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded};
use futures_util::StreamExt;
use reqwest_eventsource::Event as SseEvent;
use serde_json::{Value, json};
use tokio::runtime::Builder;

use jarvis_avatar::act::strip_act_delay_for_tts;
use jarvis_avatar::config::Settings;
use jarvis_avatar::ironclaw::client::{GatewayClient, GatewayError};
use jarvis_avatar::ironclaw::types::{
    AppEvent, ImageData, SendMessageRequest, ThreadInfo, TurnInfo, parse_app_event,
};

use super::channel_server::{
    ChatCompleteMessage, HubBroadcast, HubInputTextMessage, TtsSpeakMessage,
};
use super::home_assistant_events::AiriHaEventQueue;
use super::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};

pub struct IronclawChatPlugin;

impl Plugin for IronclawChatPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChatState>()
            .add_message::<AssistantDeltaMessage>()
            .add_message::<ThinkingStateMessage>()
            .add_message::<ToolEventMessage>()
            .add_message::<ThreadListMessage>()
            .add_message::<HistoryMessage>()
            .add_message::<ChatStatusMessage>()
            .add_message::<LocalUserEchoMessage>()
            // PostStartup so `HubBroadcast` (inserted by `ChannelHubPlugin` in
            // `Startup`) is already in the world when we spawn the gateway thread.
            .add_systems(PostStartup, spawn_gateway_thread)
            .add_systems(
                Update,
                (
                    pump_gateway_into_bevy,
                    bridge_hub_input_text,
                    update_chat_state_from_messages,
                ),
            );
    }
}

/// Fire-and-forget signal from the chat UI: "I just handed the gateway this
/// user message, please echo it into `ChatState.transcript` *now* so the
/// bubble shows up even if the LLM errors out or the roundtrip takes
/// seconds." Without this, the user types → hits Send → their message
/// vanishes until either the assistant replies (triggers history reload
/// through the SSE `Response` path) or they click the thread again.
#[derive(Message, Debug, Clone)]
#[allow(dead_code)]
pub struct LocalUserEchoMessage {
    pub text: String,
    pub attachments: usize,
}

/// Heuristic check: does this look like a gateway-surfaced error we should
/// NOT feed to TTS? We match the concrete failure modes observed:
///   * `"Error: LLM error: Provider …"` — OpenRouter-style passthrough.
///   * `"Provider <name> error: …"` — direct provider errors.
///   * Anything starting with `"Error:"` or `"Gateway error:"`.
fn is_error_like_response(text: &str) -> bool {
    let head: String = text.trim().chars().take(80).collect();
    let lowered = head.to_ascii_lowercase();
    lowered.starts_with("error:")
        || lowered.starts_with("error -")
        || lowered.starts_with("gateway error")
        || lowered.contains("llm error")
        || (lowered.starts_with("provider ") && lowered.contains(" error"))
}

// ---------- Bevy-facing types --------------------------------------------------

#[derive(Resource, Debug, Default, Clone)]
pub struct ChatState {
    pub base_url: String,
    pub has_bearer: bool,
    pub active_thread: Option<String>,
    pub threads: Vec<ThreadInfo>,
    pub thinking: Option<String>,
    /// Latest thinking text for the in-flight assistant turn; moved into the
    /// next [`TranscriptLine::thinking`] when [`ChatCompleteMessage`] arrives.
    pub thinking_buffer: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    /// Tail of recent transcript lines for the debug UI (oldest → newest).
    pub transcript: VecDeque<TranscriptLine>,
    /// Buffer of stream chunks for the *current* (in-flight) assistant turn.
    pub streaming_buffer: String,
}

#[derive(Debug, Clone)]
pub struct TranscriptLine {
    pub role: TranscriptRole,
    pub text: String,
    /// Reasoning / thinking shown above the assistant bubble when present.
    pub thinking: Option<String>,
    /// Follow-up prompts from a structured gateway/LLM envelope (`suggestions`
    /// inside a ` ```json ` block). Rendered as clickable chips in the chat UI.
    pub suggestions: Vec<String>,
    /// Pretty-printed `tool_calls` JSON from the same envelope, if any.
    pub tool_calls_json: Option<String>,
}

/// Parsed from assistant text that contains a fenced ` ```json ` block with
/// `tool_calls`, optional `response`, and optional `suggestions` (IronClaw /
/// agent tool-call pattern).
#[derive(Debug, Default)]
struct ParsedAssistantEnvelope {
    display_text: String,
    suggestions: Vec<String>,
    tool_calls_json: Option<String>,
}

/// Returns `(fence_start_byte, json_body_start_byte)` for the first ` ```json `
/// fence in `raw`.
fn find_json_code_fence(raw: &str) -> Option<(usize, usize)> {
    for pat in ["```json", "```JSON"] {
        let Some(start) = raw.find(pat) else {
            continue;
        };
        let mut i = start + pat.len();
        while i < raw.len() && matches!(raw.as_bytes()[i], b' ' | b'\t') {
            i += 1;
        }
        if raw.get(i..).is_some_and(|s| s.starts_with("\r\n")) {
            i += 2;
        } else if i < raw.len() && raw.as_bytes()[i] == b'\n' {
            i += 1;
        }
        return Some((start, i));
    }
    None
}

fn parse_gateway_assistant_content(raw: &str) -> ParsedAssistantEnvelope {
    let raw_trim = raw.trim_end();
    let Some((fence_start, json_start)) = find_json_code_fence(raw_trim) else {
        return ParsedAssistantEnvelope {
            display_text: raw_trim.to_string(),
            ..Default::default()
        };
    };
    let after_json = &raw_trim[json_start..];
    let Some(close_rel) = after_json.find("```") else {
        return ParsedAssistantEnvelope {
            display_text: raw_trim.to_string(),
            ..Default::default()
        };
    };
    let json_str = after_json[..close_rel].trim();
    let Ok(value) = serde_json::from_str::<Value>(json_str) else {
        return ParsedAssistantEnvelope {
            display_text: raw_trim.to_string(),
            ..Default::default()
        };
    };

    let looks_like_tool_envelope = value.get("tool_calls").is_some()
        || value.get("suggestions").is_some()
        || value.get("response").is_some();
    if !looks_like_tool_envelope {
        return ParsedAssistantEnvelope {
            display_text: raw_trim.to_string(),
            ..Default::default()
        };
    }

    let suggestions: Vec<String> = value
        .get("suggestions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::trim))
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let tool_calls_json = value
        .get("tool_calls")
        .and_then(|tc| serde_json::to_string_pretty(tc).ok());

    let response_text = value
        .get("response")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    let before = raw_trim[..fence_start].trim_end();
    let display_text = if response_text.is_empty() {
        if before.is_empty() {
            raw_trim.to_string()
        } else {
            before.to_string()
        }
    } else if before.is_empty() {
        response_text.to_string()
    } else {
        format!("{before}\n\n{response_text}")
    };

    ParsedAssistantEnvelope {
        display_text,
        suggestions,
        tool_calls_json,
    }
}

impl TranscriptLine {
    /// Build an assistant line from raw gateway `Response.content`, stripping
    /// structured ` ```json ` tool envelopes into [`Self::suggestions`] /
    /// [`Self::tool_calls_json`] and a reader-facing [`Self::text`] body.
    pub fn assistant_from_gateway_content(content: String, thinking: Option<String>) -> Self {
        let parsed = parse_gateway_assistant_content(&content);
        Self {
            role: TranscriptRole::Assistant,
            text: parsed.display_text,
            thinking,
            suggestions: parsed.suggestions,
            tool_calls_json: parsed.tool_calls_json,
        }
    }

    /// Same parsing as [`Self::assistant_from_gateway_content`], for an in-flight
    /// streaming buffer so the UI can show tool calls / suggestion chips before
    /// the final `Response` arrives.
    pub fn parse_raw_assistant_bubble(raw: &str) -> (String, Vec<String>, Option<String>) {
        let p = parse_gateway_assistant_content(raw);
        (p.display_text, p.suggestions, p.tool_calls_json)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TranscriptRole {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Message, Debug, Clone)]
#[allow(dead_code)]
pub struct AssistantDeltaMessage {
    pub thread_id: Option<String>,
    pub delta: String,
}

#[derive(Message, Debug, Clone)]
pub struct ThinkingStateMessage {
    pub active: bool,
    pub text: String,
}

#[derive(Message, Debug, Clone)]
#[allow(dead_code)]
pub struct ToolEventMessage {
    pub phase: ToolPhase,
    pub tool: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPhase {
    Started,
    Completed,
    Result,
}

#[derive(Message, Debug, Clone, Default)]
#[allow(dead_code)]
pub struct ThreadListMessage(pub Vec<ThreadInfo>);

#[derive(Message, Debug, Clone)]
#[allow(dead_code)]
pub struct HistoryMessage {
    pub thread_id: String,
    pub turns: Vec<TranscriptLine>,
}

#[derive(Message, Debug, Clone)]
#[allow(dead_code)]
pub struct ChatStatusMessage {
    pub status: Option<String>,
    pub error: Option<String>,
}

/// Commands the Bevy world fires at the gateway thread.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ChatCommand {
    Send {
        text: String,
        thread_id: Option<String>,
        images: Vec<ImageData>,
    },
    RefreshThreads,
    NewThread,
    SetActiveThread(String),
    LoadHistory {
        thread_id: String,
        limit: u32,
    },
}

/// Resource handle the rest of the app uses to publish [`ChatCommand`]s.
#[derive(Resource, Clone)]
pub struct GatewayClientHandle {
    tx: Sender<ChatCommand>,
}

impl GatewayClientHandle {
    pub fn send_text(&self, text: impl Into<String>, thread_id: Option<String>) {
        let _ = self.tx.send(ChatCommand::Send {
            text: text.into(),
            thread_id,
            images: Vec::new(),
        });
    }

    pub fn send_with_images(
        &self,
        text: impl Into<String>,
        thread_id: Option<String>,
        images: Vec<ImageData>,
    ) {
        let _ = self.tx.send(ChatCommand::Send {
            text: text.into(),
            thread_id,
            images,
        });
    }

    pub fn refresh_threads(&self) {
        let _ = self.tx.send(ChatCommand::RefreshThreads);
    }

    pub fn new_thread(&self) {
        let _ = self.tx.send(ChatCommand::NewThread);
    }

    pub fn set_active_thread(&self, id: impl Into<String>) {
        let _ = self.tx.send(ChatCommand::SetActiveThread(id.into()));
    }

    #[allow(dead_code)]
    pub fn load_history(&self, thread_id: impl Into<String>, limit: u32) {
        let _ = self.tx.send(ChatCommand::LoadHistory {
            thread_id: thread_id.into(),
            limit,
        });
    }
}

#[derive(Resource)]
struct GatewayInbound {
    rx: Receiver<GatewayInboundEvent>,
}

enum GatewayInboundEvent {
    /// Decoded SSE event from the gateway.
    Sse(AppEvent),
    /// Thread list refreshed (e.g. after `RefreshThreads` or boot).
    Threads(Vec<ThreadInfo>),
    /// Active thread changed (acknowledged from the worker).
    ActiveThread(Option<String>),
    /// History payload — already flattened to transcript lines.
    History {
        thread_id: String,
        turns: Vec<TranscriptLine>,
    },
    /// Transport-level state change (auth ok, auth rejected, reconnecting…).
    Status {
        status: Option<String>,
        error: Option<String>,
    },
}

// ---------- Startup / thread ---------------------------------------------------

fn spawn_gateway_thread(
    mut commands: Commands,
    settings: Res<Settings>,
    hub: Res<HubBroadcast>,
    traffic: Option<Res<TrafficLogSink>>,
) {
    let cfg = settings.gateway.clone();
    let module_name = settings.ironclaw.module_name.clone();
    let hub_tx = hub.clone();
    let traffic = traffic.map(|t| (*t).clone());

    let (cmd_tx, cmd_rx) = unbounded::<ChatCommand>();
    let (in_tx, in_rx) = unbounded::<GatewayInboundEvent>();

    commands.insert_resource(GatewayClientHandle { tx: cmd_tx });
    commands.insert_resource(GatewayInbound { rx: in_rx });
    commands.insert_resource(ChatState {
        base_url: cfg.base_url.clone(),
        has_bearer: !cfg.auth_token.is_empty(),
        ..Default::default()
    });

    thread::Builder::new()
        .name("jarvis-gateway".into())
        .spawn(move || {
            let rt = match Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("gateway tokio runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(run_gateway(cfg, module_name, cmd_rx, in_tx, hub_tx, traffic));
        })
        .expect("failed to spawn jarvis-gateway thread");
}

async fn run_gateway(
    cfg: jarvis_avatar::config::GatewaySettings,
    module_name: String,
    cmd_rx: Receiver<ChatCommand>,
    in_tx: Sender<GatewayInboundEvent>,
    hub: HubBroadcast,
    traffic: Option<TrafficLogSink>,
) {
    let client = Arc::new(GatewayClient::new(
        &cfg.base_url,
        &cfg.auth_token,
        cfg.request_timeout_ms,
    ));
    let traffic_log = traffic.clone();
    let active_thread = Arc::new(tokio::sync::RwLock::new({
        let s = cfg.default_thread_id.trim();
        if s.is_empty() { None } else { Some(s.to_string()) }
    }));

    // Boot: list threads, materialize an active one if needed.
    bootstrap_threads(&client, &active_thread, &in_tx, traffic_log.as_ref()).await;

    // Start the SSE listener task.
    {
        let client = Arc::clone(&client);
        let in_tx = in_tx.clone();
        let active = Arc::clone(&active_thread);
        let module_name = module_name.clone();
        let hub = hub.clone();
        let traffic_es = traffic.clone();
        tokio::spawn(async move {
            run_event_stream(client, active, in_tx, hub, module_name, traffic_es).await;
        });
    }

    // Drain the command channel.
    let history_limit = cfg.history_limit;
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
            ChatCommand::Send {
                text,
                thread_id,
                images,
            } => {
                let resolved = match thread_id {
                    Some(t) => Some(t),
                    None => active_thread.read().await.clone(),
                };
                let body = SendMessageRequest {
                    content: text,
                    thread_id: resolved,
                    timezone: None,
                    images,
                };
                if let Some(ref l) = traffic {
                    l.push(
                        TrafficChannel::IronclawGatewayHttp,
                        TrafficDirection::Outbound,
                        "POST /api/chat/send",
                        Some(json!({
                            "contentPreview": body.content.chars().take(400).collect::<String>(),
                            "threadId": body.thread_id,
                            "imageAttachments": body.images.len(),
                        })),
                    );
                }
                match client.send_message(&body).await {
                    Ok(resp) => {
                        let _ = in_tx.send(GatewayInboundEvent::Status {
                            status: Some(format!("queued ({})", resp.status)),
                            error: None,
                        });
                    }
                    Err(e) => report_error(&in_tx, "send_message", e),
                }
            }
            ChatCommand::RefreshThreads => {
                if let Some(ref l) = traffic {
                    l.push(
                        TrafficChannel::IronclawGatewayHttp,
                        TrafficDirection::Outbound,
                        "GET /api/chat/threads",
                        None,
                    );
                }
                match client.list_threads().await {
                    Ok(list) => {
                        let mut all = Vec::new();
                        if let Some(t) = list.assistant_thread {
                            all.push(t);
                        }
                        all.extend(list.threads.into_iter());
                        let _ = in_tx.send(GatewayInboundEvent::Threads(all));
                    }
                    Err(e) => report_error(&in_tx, "list_threads", e),
                }
            }
            ChatCommand::NewThread => {
                if let Some(ref l) = traffic {
                    l.push(
                        TrafficChannel::IronclawGatewayHttp,
                        TrafficDirection::Outbound,
                        "POST /api/chat/thread (new)",
                        None,
                    );
                }
                match client.create_thread().await {
                    Ok(thread) => {
                        let id = thread.id.clone();
                        *active_thread.write().await = Some(id.clone());
                        let _ = in_tx.send(GatewayInboundEvent::ActiveThread(Some(id)));
                        // Refresh the thread list so the new entry shows up.
                        if let Ok(list) = client.list_threads().await {
                            let mut all = Vec::new();
                            if let Some(t) = list.assistant_thread {
                                all.push(t);
                            }
                            all.extend(list.threads.into_iter());
                            let _ = in_tx.send(GatewayInboundEvent::Threads(all));
                        }
                    }
                    Err(e) => report_error(&in_tx, "create_thread", e),
                }
            }
            ChatCommand::SetActiveThread(id) => {
                if let Some(ref l) = traffic {
                    l.push(
                        TrafficChannel::IronclawGatewayHttp,
                        TrafficDirection::Outbound,
                        format!("set_active_thread id={id}"),
                        None,
                    );
                }
                *active_thread.write().await = Some(id.clone());
                let _ = in_tx.send(GatewayInboundEvent::ActiveThread(Some(id.clone())));
                // Auto-load recent history when switching.
                load_history_inner(&client, &id, history_limit, &in_tx).await;
            }
            ChatCommand::LoadHistory { thread_id, limit } => {
                if let Some(ref l) = traffic {
                    l.push(
                        TrafficChannel::IronclawGatewayHttp,
                        TrafficDirection::Outbound,
                        format!("GET /api/chat/history (thread={thread_id}, limit={limit})"),
                        None,
                    );
                }
                load_history_inner(&client, &thread_id, limit, &in_tx).await;
            }
        }
    }
}

async fn bootstrap_threads(
    client: &GatewayClient,
    active_thread: &Arc<tokio::sync::RwLock<Option<String>>>,
    in_tx: &Sender<GatewayInboundEvent>,
    traffic: Option<&TrafficLogSink>,
) {
    if let Some(log) = traffic {
        log.push(
            TrafficChannel::IronclawGatewayHttp,
            TrafficDirection::Outbound,
            "GET /api/chat/threads (bootstrap)",
            None,
        );
    }
    let list = match client.list_threads().await {
        Ok(l) => l,
        Err(e) => {
            report_error(in_tx, "list_threads (boot)", e);
            return;
        }
    };

    let mut all = Vec::new();
    if let Some(t) = list.assistant_thread.clone() {
        all.push(t);
    }
    all.extend(list.threads.iter().cloned());

    {
        let mut guard = active_thread.write().await;
        if guard.is_none() {
            *guard = list.active_thread.clone().or_else(|| all.first().map(|t| t.id.clone()));
        }
        // If the configured default no longer exists, drop it.
        if let Some(id) = guard.clone() {
            if !all.iter().any(|t| t.id == id) {
                *guard = all.first().map(|t| t.id.clone());
            }
        }
    }

    // If we still don't have an active thread, mint a fresh one.
    let active = active_thread.read().await.clone();
    let active = if active.is_none() {
        match client.create_thread().await {
            Ok(thread) => {
                let id = thread.id.clone();
                all.push(thread);
                *active_thread.write().await = Some(id.clone());
                Some(id)
            }
            Err(e) => {
                report_error(in_tx, "create_thread (boot)", e);
                None
            }
        }
    } else {
        active
    };

    let _ = in_tx.send(GatewayInboundEvent::Threads(all));
    let _ = in_tx.send(GatewayInboundEvent::ActiveThread(active));
    let _ = in_tx.send(GatewayInboundEvent::Status {
        status: Some("connected".into()),
        error: None,
    });
}

fn turn_thinking_for_history(turn: &TurnInfo) -> Option<String> {
    let t = turn.thinking.as_deref()?.trim();
    (!t.is_empty()).then(|| t.to_string())
}

async fn load_history_inner(
    client: &GatewayClient,
    thread_id: &str,
    limit: u32,
    in_tx: &Sender<GatewayInboundEvent>,
) {
    match client.history(thread_id, Some(limit), None).await {
        Ok(h) => {
            let mut lines = Vec::with_capacity(h.turns.len() * 2);
            for turn in h.turns {
                let assistant_thinking = turn_thinking_for_history(&turn);
                lines.push(TranscriptLine {
                    role: TranscriptRole::User,
                    text: turn.user_input,
                    thinking: None,
                    suggestions: vec![],
                    tool_calls_json: None,
                });
                if let Some(resp) = turn.response {
                    lines.push(TranscriptLine::assistant_from_gateway_content(
                        resp,
                        assistant_thinking,
                    ));
                }
            }
            let _ = in_tx.send(GatewayInboundEvent::History {
                thread_id: h.thread_id,
                turns: lines,
            });
        }
        Err(e) => report_error(in_tx, "history", e),
    }
}

fn report_error(in_tx: &Sender<GatewayInboundEvent>, op: &str, e: GatewayError) {
    warn!("gateway {op}: {e}");
    let _ = in_tx.send(GatewayInboundEvent::Status {
        status: None,
        error: Some(format!("{op}: {e}")),
    });
}

async fn run_event_stream(
    client: Arc<GatewayClient>,
    active_thread: Arc<tokio::sync::RwLock<Option<String>>>,
    in_tx: Sender<GatewayInboundEvent>,
    hub: HubBroadcast,
    module_name: String,
    traffic: Option<TrafficLogSink>,
) {
    let mut last_event_id: Option<String> = None;
    let mut backoff_ms: u64 = 1_000;

    loop {
        let mut es = client.open_event_stream(last_event_id.as_deref());
        let _ = in_tx.send(GatewayInboundEvent::Status {
            status: Some("sse: connecting".into()),
            error: None,
        });

        while let Some(item) = es.next().await {
            match item {
                Ok(SseEvent::Open) => {
                    backoff_ms = 1_000;
                    let _ = in_tx.send(GatewayInboundEvent::Status {
                        status: Some("sse: open".into()),
                        error: None,
                    });
                }
                Ok(SseEvent::Message(msg)) => {
                    if let Some(ref log) = traffic {
                        let parsed: Value = serde_json::from_str(&msg.data).unwrap_or_else(|_| {
                            json!({ "raw": msg.data.chars().take(4000).collect::<String>() })
                        });
                        log.push(
                            TrafficChannel::IronclawGatewaySse,
                            TrafficDirection::Inbound,
                            format!(
                                "sse event={} id={}",
                                msg.event.trim(),
                                msg.id.chars().take(32).collect::<String>()
                            ),
                            Some(parsed),
                        );
                    }
                    if !msg.id.is_empty() {
                        last_event_id = Some(msg.id.clone());
                    }
                    match parse_app_event(&msg.data) {
                        Ok(ev) => {
                            // Route AppEvent::Response to the channel hub
                            // BEFORE forwarding to Bevy, so peers see the
                            // canonical envelope without depending on Bevy
                            // tick timing.
                            if let AppEvent::Response { content, thread_id } = &ev {
                                publish_chat_complete(&hub, &module_name, content, thread_id);
                            }
                            // Filter SSE events to the active thread when one
                            // is set (the gateway can broadcast events for
                            // threads we don't care about).
                            let active = active_thread.read().await.clone();
                            if let (Some(active), Some(ev_thread)) = (active.as_deref(), ev.thread_id()) {
                                if active != ev_thread {
                                    continue;
                                }
                            }
                            let _ = in_tx.send(GatewayInboundEvent::Sse(ev));
                        }
                        Err((e, fallback)) => {
                            warn!(
                                "sse parse error: {e}; raw type={:?}",
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
                    let _ = in_tx.send(GatewayInboundEvent::Status {
                        status: None,
                        error: Some(format!("sse: {e}")),
                    });
                    if auth_failed {
                        warn!("sse auth rejected; stopping reconnect loop");
                        return;
                    }
                    break;
                }
            }
        }

        // Stream ended (server closed or transport died). Backoff + retry.
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms.saturating_mul(2)).min(30_000);
    }
}

fn publish_chat_complete(hub: &HubBroadcast, module_name: &str, content: &str, thread_id: &str) {
    let envelope = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "from": module_name,
        "thread_id": thread_id,
        "message": {
            "role": "assistant",
            "content": content,
        },
    });
    hub.send("output:gen-ai:chat:complete", envelope);
}

// ---------- Bevy systems -------------------------------------------------------

fn pump_gateway_into_bevy(
    inbound: Option<Res<GatewayInbound>>,
    mut delta: MessageWriter<AssistantDeltaMessage>,
    mut thinking: MessageWriter<ThinkingStateMessage>,
    mut tools: MessageWriter<ToolEventMessage>,
    mut threads: MessageWriter<ThreadListMessage>,
    mut history: MessageWriter<HistoryMessage>,
    mut status: MessageWriter<ChatStatusMessage>,
    mut chat_complete: MessageWriter<ChatCompleteMessage>,
    mut tts: MessageWriter<TtsSpeakMessage>,
    mut state: ResMut<ChatState>,
) {
    let Some(inbound) = inbound else { return };
    while let Ok(ev) = inbound.rx.try_recv() {
        match ev {
            GatewayInboundEvent::Sse(app_event) => {
                handle_app_event(
                    app_event,
                    &mut delta,
                    &mut thinking,
                    &mut tools,
                    &mut chat_complete,
                    &mut tts,
                );
            }
            GatewayInboundEvent::Threads(list) => {
                state.threads = list.clone();
                threads.write(ThreadListMessage(list));
            }
            GatewayInboundEvent::ActiveThread(id) => {
                state.active_thread = id;
            }
            GatewayInboundEvent::History { thread_id, turns } => {
                state.transcript.clear();
                for line in turns.iter().cloned() {
                    push_transcript(&mut state, line);
                }
                history.write(HistoryMessage { thread_id, turns });
            }
            GatewayInboundEvent::Status { status: s, error } => {
                if let Some(ref msg) = error {
                    state.last_error = Some(msg.clone());
                } else {
                    // Clear stale transport errors (e.g. transient SSE decode failures)
                    // when the gateway worker reports a healthy update (`sse: open`, etc.).
                    state.last_error = None;
                }
                if let Some(ref msg) = s {
                    state.last_status = Some(msg.clone());
                }
                status.write(ChatStatusMessage { status: s, error });
            }
        }
    }
}

fn handle_app_event(
    ev: AppEvent,
    delta: &mut MessageWriter<AssistantDeltaMessage>,
    thinking: &mut MessageWriter<ThinkingStateMessage>,
    tools: &mut MessageWriter<ToolEventMessage>,
    chat_complete: &mut MessageWriter<ChatCompleteMessage>,
    tts: &mut MessageWriter<TtsSpeakMessage>,
) {
    match ev {
        AppEvent::StreamChunk { content, thread_id } => {
            delta.write(AssistantDeltaMessage {
                thread_id,
                delta: content,
            });
        }
        AppEvent::Thinking { message, .. } => {
            thinking.write(ThinkingStateMessage {
                active: true,
                text: message,
            });
        }
        AppEvent::Response { content, .. } => {
            // Final turn — drive expressions + TTS through the existing
            // contract so `expressions.rs` / `tts.rs` don't need to know
            // about the gateway path.
            let speak = strip_act_delay_for_tts(&content).to_string();
            chat_complete.write(ChatCompleteMessage {
                content: content.clone(),
            });
            // Skip TTS on LLM error strings. The gateway surfaces provider
            // failures as a normal `Response` (so the user still sees the
            // failure in-chat) — which previously got routed straight to
            // Kokoro. Kokoro's Chinese-glyph fallback of a long "Error: LLM
            // error: Provider …" string produced a ~1MB WAV that bevy_audio
            // refused to decode, panicking the render loop.
            let looks_like_error = is_error_like_response(&speak);
            if looks_like_error {
                warn!(
                    target: "ironclaw_chat",
                    "skipping TTS for error-like response ({} chars)",
                    speak.len()
                );
            }
            if !speak.trim().is_empty() && !looks_like_error {
                tts.write(TtsSpeakMessage { text: speak });
            }
            thinking.write(ThinkingStateMessage {
                active: false,
                text: String::new(),
            });
        }
        AppEvent::Status { message, .. } => {
            // Surfaced as a "status" tool-event so the debug UI can show it
            // without us inventing a third message type.
            tools.write(ToolEventMessage {
                phase: ToolPhase::Result,
                tool: "status".into(),
                payload: json!({ "message": message }),
            });
        }
        AppEvent::ToolStarted {
            name,
            detail,
            thread_id,
        } => {
            tools.write(ToolEventMessage {
                phase: ToolPhase::Started,
                tool: name,
                payload: json!({ "detail": detail, "thread_id": thread_id }),
            });
        }
        AppEvent::ToolCompleted {
            name,
            success,
            error,
            thread_id,
        } => {
            tools.write(ToolEventMessage {
                phase: ToolPhase::Completed,
                tool: name,
                payload: json!({ "success": success, "error": error, "thread_id": thread_id }),
            });
        }
        AppEvent::ToolResult {
            name,
            preview,
            thread_id,
        } => {
            tools.write(ToolEventMessage {
                phase: ToolPhase::Result,
                tool: name,
                payload: json!({ "preview": preview, "thread_id": thread_id }),
            });
        }
        AppEvent::Error { message, .. } => {
            tools.write(ToolEventMessage {
                phase: ToolPhase::Result,
                tool: "error".into(),
                payload: json!({ "message": message }),
            });
            thinking.write(ThinkingStateMessage {
                active: false,
                text: String::new(),
            });
        }
        AppEvent::Other => {}
    }
}

/// Forwards `input:text` envelopes (typically from `server.mjs`'s voice
/// pipeline) to the gateway as a normal chat message.
fn bridge_hub_input_text(
    mut events: MessageReader<HubInputTextMessage>,
    handle: Option<Res<GatewayClientHandle>>,
    state: Res<ChatState>,
    mut airi_events: Option<ResMut<AiriHaEventQueue>>,
) {
    let Some(handle) = handle else { return };
    for ev in events.read() {
        let text = ev.text.trim();
        if text.is_empty() {
            continue;
        }
        let outbound_text = if let Some(queue) = airi_events.as_deref_mut() {
            if let Some(ctx) = queue.take_context_block() {
                format!("{ctx}\n\nUser: {text}")
            } else {
                text.to_string()
            }
        } else {
            text.to_string()
        };
        info!(
            "hub→gateway: forwarding input:text from '{}' ({} chars)",
            ev.source,
            text.len()
        );
        handle.send_text(outbound_text, state.active_thread.clone());
    }
}

/// Mirrors gateway-side messages back into [`ChatState`] so the debug UI's
/// transcript and indicators are tick-current without each panel having to
/// maintain its own copy.
fn update_chat_state_from_messages(
    mut state: ResMut<ChatState>,
    mut deltas: MessageReader<AssistantDeltaMessage>,
    mut thinking: MessageReader<ThinkingStateMessage>,
    mut chat_complete: MessageReader<ChatCompleteMessage>,
    mut hub_inputs: MessageReader<HubInputTextMessage>,
    mut local_echo: MessageReader<LocalUserEchoMessage>,
) {
    for ev in deltas.read() {
        state.streaming_buffer.push_str(&ev.delta);
    }
    for ev in chat_complete.read() {
        let thinking = state.thinking_buffer.take();
        let line = TranscriptLine::assistant_from_gateway_content(ev.content.clone(), thinking);
        push_transcript(&mut state, line);
        state.streaming_buffer.clear();
    }
    for ev in thinking.read() {
        if ev.active {
            state.thinking_buffer = Some(ev.text.clone());
            state.thinking = Some(ev.text.clone());
        } else {
            state.thinking = None;
            state.thinking_buffer = None;
        }
    }
    for ev in hub_inputs.read() {
        let text = ev.text.trim();
        if text.is_empty() {
            continue;
        }
        let label = if ev.source.is_empty() {
            text.to_string()
        } else {
            format!("[{}] {}", ev.source, text)
        };
        push_transcript(
            &mut state,
            TranscriptLine {
                role: TranscriptRole::User,
                text: label,
                thinking: None,
                suggestions: vec![],
                tool_calls_json: None,
            },
        );
    }
    for ev in local_echo.read() {
        // Attachments aren't rendered in-bubble (the transcript is text-only
        // for now); surface the count so the user sees a confirmation line
        // even when their "message" was images only.
        let text = if ev.text.is_empty() && ev.attachments > 0 {
            format!(
                "[{} image{}]",
                ev.attachments,
                if ev.attachments == 1 { "" } else { "s" }
            )
        } else if ev.attachments > 0 {
            format!(
                "{} [+{} image{}]",
                ev.text,
                ev.attachments,
                if ev.attachments == 1 { "" } else { "s" }
            )
        } else {
            ev.text.clone()
        };
        if text.trim().is_empty() {
            continue;
        }
        push_transcript(
            &mut state,
            TranscriptLine {
                role: TranscriptRole::User,
                text,
                thinking: None,
                suggestions: vec![],
                tool_calls_json: None,
            },
        );
    }
}

fn push_transcript(state: &mut ChatState, line: TranscriptLine) {
    const MAX_LINES: usize = 200;
    state.transcript.push_back(line);
    while state.transcript.len() > MAX_LINES {
        state.transcript.pop_front();
    }
}
