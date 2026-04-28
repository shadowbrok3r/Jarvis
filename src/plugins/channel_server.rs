//! Phase 1 (corrected): IronClaw-style channel-server hub.
//!
//! `jarvis-avatar` HOSTS the WebSocket hub that AIRI's `server-runtime` used to host on
//! port 6121. `server.mjs` (ha-voice-bridge), `ironclaw-proxy`, and any other module
//! connect to us. Wire protocol is unchanged (raw `{type,data,metadata}` envelopes).
//!
//! Surfaces on `settings.ironclaw.bind_address` (default `0.0.0.0:6121`):
//!
//! * `GET  /ws`         — WebSocket. On connect: expect `module:authenticate` then
//!                        `module:announce`. Any other envelope is fanned out to every
//!                        other authenticated peer AND consumed locally by Bevy.
//! * `POST /broadcast`  — JSON body `{"type":"...","data":{...}}`. Same semantics as
//!                        receiving the envelope over WS: fan out + locally consume.
//!                        Use this from `ironclaw-proxy` to push AI chat completions
//!                        without opening a WS client.
//! * `GET  /health`     — JSON peer roster for ops / the debug UI.
//! * `GET  /jarvis-ios/v1/manifest` — JSON profile snapshot for the iOS companion (Bearer token when set).
//! * `GET  /jarvis-ios/v1/asset/{*path}` — Raw bytes under `./assets/` (path traversal rejected).
//! * `GET  /jarvis-ios/v1/config/spring-presets/{name}` — Preset TOML (`xxxxxxxxxxxxxxxx.toml` only).
//!
//! Emits the same Bevy `Message`s the old WS client did so downstream plugins
//! (`expressions`, `look_at`, `tts`, `debug_ui`) stay on the same contract:
//!   * [`ChatCompleteMessage`]  — `output:gen-ai:chat:complete`
//!   * [`LookAtRequestMessage`] — `vrm:set-look-at`
//!   * [`TtsSpeakMessage`]      — ACT/DELAY-stripped TTS text
//!   * [`WsIncomingMessage`]    — catch-all for everything else

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::header;
use axum::http::{HeaderMap, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use bevy::prelude::*;
use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::runtime::Builder;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use jarvis_avatar::act::strip_act_delay_for_tts;
use jarvis_avatar::config::Settings;
use jarvis_avatar::ironclaw::protocol::EnvelopeBody;

use super::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};

pub struct ChannelHubPlugin;

impl Plugin for ChannelHubPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HubState>()
            .add_message::<ChatCompleteMessage>()
            .add_message::<LookAtRequestMessage>()
            .add_message::<TtsSpeakMessage>()
            .add_message::<HubInputTextMessage>()
            .add_message::<WsIncomingMessage>()
            .add_systems(Startup, spawn_hub_thread)
            .add_systems(Update, pump_hub_into_bevy);
    }
}

// ---------- Bevy-facing types --------------------------------------------------

/// Hub liveness visible to Bevy systems / debug UI.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct HubState {
    pub peer_count: usize,
    pub bound_to: Option<String>,
}

#[derive(Message, Debug, Clone)]
pub struct ChatCompleteMessage {
    pub content: String,
}

#[derive(Message, Debug, Clone)]
pub struct LookAtRequestMessage {
    /// Local target position (relative to VRM root) in meters. `None` = revert to cursor.
    pub local_target: Option<Vec3>,
}

#[derive(Message, Debug, Clone)]
pub struct TtsSpeakMessage {
    pub text: String,
}

#[derive(Message, Debug, Clone)]
#[allow(dead_code)]
pub struct WsIncomingMessage {
    pub envelope: EnvelopeBody,
}

/// `input:text` envelope received from a peer (typically `server.mjs`'s
/// HA voice pipeline). The IronClaw chat plugin consumes these and routes them
/// to the gateway as a chat message.
#[derive(Message, Debug, Clone)]
pub struct HubInputTextMessage {
    pub text: String,
    /// Hint about who produced the input (e.g. `"ha-voice-bridge"`). Mostly
    /// used for logging / threading hints; empty when unknown.
    pub source: String,
}

/// Handle for Bevy systems (and the debug UI) to publish envelopes onto the hub.
/// The hub authors the `metadata.source` using `settings.ironclaw.module_name`.
/// Also exposes a Tokio `broadcast::Sender` mirror of every incoming envelope
/// so Tokio-side consumers (MCP tool handlers, Kimodo awaiter) can subscribe
/// without going through Bevy.
#[derive(Resource, Clone)]
pub struct HubBroadcast {
    tx: Sender<OutboundFrame>,
    inbound: tokio::sync::broadcast::Sender<EnvelopeBody>,
}

impl HubBroadcast {
    /// Fan out an arbitrary `{type,data}` envelope to every connected peer.
    pub fn send(&self, kind: impl Into<String>, data: Value) {
        let _ = self.tx.send(OutboundFrame::Typed {
            kind: kind.into(),
            data,
            event_id: None,
        });
    }

    /// Like [`Self::send`] but pins `metadata.event.id` to a caller-supplied value
    /// so responding peers (e.g. Kimodo) can echo it back inside `data.requestId`
    /// and we can correlate status updates.
    pub fn send_with_event_id(
        &self,
        kind: impl Into<String>,
        data: Value,
        event_id: impl Into<String>,
    ) -> String {
        let event_id: String = event_id.into();
        let _ = self.tx.send(OutboundFrame::Typed {
            kind: kind.into(),
            data,
            event_id: Some(event_id.clone()),
        });
        event_id
    }

    /// Convenience: publish `input:text` (the frame `server.mjs` would normally send us).
    pub fn send_input_text(&self, text: impl Into<String>, module_name: &str) {
        self.send(
            "input:text",
            json!({ "text": text.into(), "source": module_name }),
        );
    }

    /// Subscribe to every inbound envelope. Backed by `tokio::sync::broadcast`
    /// so slow consumers merely drop (lagged) rather than blocking the hub.
    pub fn subscribe_incoming(&self) -> tokio::sync::broadcast::Receiver<EnvelopeBody> {
        self.inbound.subscribe()
    }
}

#[derive(Resource)]
struct HubInbound {
    rx: Receiver<Inbound>,
}

enum OutboundFrame {
    Typed {
        kind: String,
        data: Value,
        event_id: Option<String>,
    },
}

enum Inbound {
    PeerCount(usize),
    Envelope(EnvelopeBody),
}

// ---------- startup / thread ---------------------------------------------------

fn spawn_hub_thread(
    mut commands: Commands,
    settings: Res<Settings>,
    traffic: Option<Res<TrafficLogSink>>,
) {
    let bind = settings.ironclaw.bind_address.clone();
    let module_name = settings.ironclaw.module_name.clone();
    let auth_token = settings.ironclaw.auth_token.clone();
    let traffic = traffic.map(|t| (*t).clone());

    let (tx_out, rx_out) = unbounded::<OutboundFrame>();
    let (tx_in, rx_in) = unbounded::<Inbound>();
    let (tx_bcast, _rx_bcast) = tokio::sync::broadcast::channel::<EnvelopeBody>(256);

    let jarvis_ios = std::sync::Arc::new(super::jarvis_ios_hub::JarvisIosHubProfile::from_settings(
        &*settings,
    ));

    commands.insert_resource(HubBroadcast {
        tx: tx_out,
        inbound: tx_bcast.clone(),
    });
    commands.insert_resource(HubInbound { rx: rx_in });
    commands.insert_resource(HubState {
        peer_count: 0,
        bound_to: Some(bind.clone()),
    });

    thread::Builder::new()
        .name("jarvis-hub".into())
        .spawn(move || {
            let rt = match Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("tokio runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(run_hub(
                bind,
                module_name,
                auth_token,
                rx_out,
                tx_in,
                tx_bcast,
                traffic,
                jarvis_ios,
            ));
        })
        .expect("failed to spawn jarvis-hub thread");
}

// ---------- axum hub -----------------------------------------------------------

#[derive(Clone)]
struct HubShared {
    peers: Arc<RwLock<HashMap<Uuid, Peer>>>,
    bevy_tx: Sender<Inbound>,
    inbound_bcast: tokio::sync::broadcast::Sender<EnvelopeBody>,
    module_name: String,
    auth_token: String,
    traffic: Option<TrafficLogSink>,
    jarvis_ios: std::sync::Arc<super::jarvis_ios_hub::JarvisIosHubProfile>,
}

struct Peer {
    tx: mpsc::UnboundedSender<String>,
    authenticated: bool,
    name: Option<String>,
    identity: Value,
}

async fn run_hub(
    bind: String,
    module_name: String,
    auth_token: String,
    rx_out: Receiver<OutboundFrame>,
    tx_in: Sender<Inbound>,
    inbound_bcast: tokio::sync::broadcast::Sender<EnvelopeBody>,
    traffic: Option<TrafficLogSink>,
    jarvis_ios: std::sync::Arc<super::jarvis_ios_hub::JarvisIosHubProfile>,
) {
    let shared = HubShared {
        peers: Arc::new(RwLock::new(HashMap::new())),
        bevy_tx: tx_in,
        inbound_bcast,
        module_name: module_name.clone(),
        auth_token,
        traffic,
        jarvis_ios,
    };

    // Drain Bevy→peers crossbeam queue and fan out over the WS peer map.
    {
        let shared = shared.clone();
        tokio::spawn(async move {
            loop {
                match rx_out.try_recv() {
                    Ok(OutboundFrame::Typed {
                        kind,
                        data,
                        event_id,
                    }) => {
                        if let Some(ref log) = shared.traffic {
                            log.push(
                                TrafficChannel::ChannelHubWsOutbound,
                                TrafficDirection::Outbound,
                                format!("fan-out type={kind}"),
                                Some(json!({ "data": data, "event_id": event_id })),
                            );
                        }
                        let frame =
                            encode_frame_with_id(&kind, data, &shared.module_name, event_id);
                        broadcast_to_all(&shared, frame, None).await;
                    }
                    Err(TryRecvError::Empty) => {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                    Err(TryRecvError::Disconnected) => return,
                }
            }
        });
    }

    let app = Router::new()
        .route("/ws", any(ws_handler))
        .route("/broadcast", post(broadcast_handler))
        .route("/health", get(health_handler))
        .route("/jarvis-ios/v1/manifest", get(jarvis_ios_manifest_handler))
        .route("/jarvis-ios/v1/asset/{*path}", get(jarvis_ios_asset_handler))
        .route(
            "/jarvis-ios/v1/config/spring-presets/{name}",
            get(jarvis_ios_spring_preset_handler),
        )
        .with_state(shared);

    let addr: SocketAddr = match bind.parse() {
        Ok(a) => a,
        Err(e) => {
            error!("invalid ironclaw.bind_address '{bind}': {e}");
            return;
        }
    };
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("hub bind {bind} failed: {e}");
            return;
        }
    };
    info!("channel-hub listening on {bind} (ws /ws · http /broadcast · /health · /jarvis-ios/v1/…)");
    if let Err(e) = axum::serve(listener, app).await {
        error!("axum serve exited: {e}");
    }
}

// ---------- HTTP endpoints -----------------------------------------------------

async fn health_handler(State(shared): State<HubShared>) -> impl IntoResponse {
    let peers = shared.peers.read().await;
    Json(json!({
        "ok": true,
        "module_name": shared.module_name,
        "peer_count": peers.len(),
        "peers": peers.values().map(|p| json!({
            "name": p.name,
            "authenticated": p.authenticated,
            "identity": p.identity,
        })).collect::<Vec<_>>(),
    }))
}

async fn jarvis_ios_manifest_handler(
    State(shared): State<HubShared>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let auth = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());
    if !super::jarvis_ios_hub::http_authorized(&shared.auth_token, auth) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(shared.jarvis_ios.manifest_json()).into_response()
}

async fn jarvis_ios_asset_handler(
    State(shared): State<HubShared>,
    Path(rel): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let auth = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());
    if !super::jarvis_ios_hub::http_authorized(&shared.auth_token, auth) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(path) = super::jarvis_ios_hub::resolve_asset_file(&rel) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let ct = super::jarvis_ios_hub::content_type_for_path(&path);
            match Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, ct)
                .body(Body::from(bytes))
            {
                Ok(r) => r.into_response(),
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn jarvis_ios_spring_preset_handler(
    State(shared): State<HubShared>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let auth = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());
    if !super::jarvis_ios_hub::http_authorized(&shared.auth_token, auth) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(path) = super::jarvis_ios_hub::resolve_spring_preset_file(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let ct = super::jarvis_ios_hub::content_type_for_path(&path);
            match Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, ct)
                .body(Body::from(bytes))
            {
                Ok(r) => r.into_response(),
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn broadcast_handler(
    State(shared): State<HubShared>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let kind = body
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if kind.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing 'type'").into_response();
    }
    let data = body.get("data").cloned().unwrap_or(Value::Null);

    let envelope = EnvelopeBody {
        message_type: kind.clone(),
        data: data.clone(),
        metadata: json!({
            "event": { "id": Uuid::new_v4().to_string() },
            "source": { "kind": "http", "id": shared.module_name },
        }),
    };
    if let Some(ref log) = shared.traffic {
        if let Ok(v) = serde_json::to_value(&envelope) {
            log.push(
                TrafficChannel::ChannelHubHttpBroadcast,
                TrafficDirection::Inbound,
                format!("POST /broadcast type={kind}"),
                Some(v),
            );
        }
    }
    let _ = shared.inbound_bcast.send(envelope.clone());
    let _ = shared.bevy_tx.send(Inbound::Envelope(envelope));
    let frame = encode_frame(&kind, data, &shared.module_name);
    broadcast_to_all(&shared, frame, None).await;

    (StatusCode::OK, "ok").into_response()
}

// ---------- WebSocket peer -----------------------------------------------------

async fn ws_handler(ws: WebSocketUpgrade, State(shared): State<HubShared>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_peer(socket, shared))
}

async fn handle_peer(mut socket: WebSocket, shared: HubShared) {
    let peer_id = Uuid::new_v4();
    let (tx_out, mut rx_out) = mpsc::unbounded_channel::<String>();

    {
        let mut peers = shared.peers.write().await;
        peers.insert(
            peer_id,
            Peer {
                tx: tx_out,
                // If no auth token is configured, everyone is implicitly trusted.
                authenticated: shared.auth_token.is_empty(),
                name: None,
                identity: Value::Null,
            },
        );
        let n = peers.len();
        let _ = shared.bevy_tx.send(Inbound::PeerCount(n));
    }
    info!("hub: peer {peer_id} connected");

    loop {
        tokio::select! {
            maybe_in = socket.recv() => {
                match maybe_in {
                    Some(Ok(WsMessage::Text(text))) => {
                        handle_peer_text(&shared, peer_id, text.as_ref()).await;
                    }
                    Some(Ok(WsMessage::Binary(bytes))) => {
                        if let Ok(text) = std::str::from_utf8(&bytes) {
                            handle_peer_text(&shared, peer_id, text).await;
                        }
                    }
                    Some(Ok(WsMessage::Ping(p))) => {
                        if socket.send(WsMessage::Pong(p)).await.is_err() { break; }
                    }
                    Some(Ok(WsMessage::Close(_))) => break,
                    Some(Err(e)) => { warn!("ws read error: {e}"); break; }
                    None => break,
                    _ => {}
                }
            }
            maybe_out = rx_out.recv() => {
                match maybe_out {
                    Some(text) => {
                        if socket.send(WsMessage::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    {
        let mut peers = shared.peers.write().await;
        peers.remove(&peer_id);
        let _ = shared.bevy_tx.send(Inbound::PeerCount(peers.len()));
    }
    info!("hub: peer {peer_id} disconnected");
}

async fn handle_peer_text(shared: &HubShared, peer_id: Uuid, text: &str) {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };
    // Accept raw {type,data,metadata} and superjson {json:{…},meta:{}} interchangeably.
    let envelope_value = value.get("json").cloned().unwrap_or(value);
    let envelope: EnvelopeBody = match serde_json::from_value(envelope_value) {
        Ok(e) => e,
        Err(_) => return,
    };

    if let Some(ref log) = shared.traffic {
        if envelope.message_type != "transport:connection:heartbeat" {
            if let Ok(v) = serde_json::to_value(&envelope) {
                log.push(
                    TrafficChannel::ChannelHubWsInbound,
                    TrafficDirection::Inbound,
                    format!("type={} peer={}", envelope.message_type, peer_id),
                    Some(v),
                );
            }
        }
    }

    match envelope.message_type.as_str() {
        "module:authenticate" => {
            let client_token = envelope
                .data
                .get("token")
                .and_then(Value::as_str)
                .unwrap_or("");
            let ok = shared.auth_token.is_empty() || client_token == shared.auth_token;

            let peers = shared.peers.read().await;
            if let Some(peer) = peers.get(&peer_id) {
                if ok {
                    let reply =
                        encode_frame("module:authenticated", json!({}), &shared.module_name);
                    let _ = peer.tx.send(reply);
                } else {
                    let reply = encode_frame(
                        "error",
                        json!({
                            "code": "invalid_token",
                            "message": "authentication rejected",
                        }),
                        &shared.module_name,
                    );
                    let _ = peer.tx.send(reply);
                }
            }
            drop(peers);

            if ok {
                let mut peers = shared.peers.write().await;
                if let Some(peer) = peers.get_mut(&peer_id) {
                    peer.authenticated = true;
                }
            }
        }

        "module:announce" => {
            let name = envelope
                .data
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let identity = envelope.data.get("identity").cloned().unwrap_or(Value::Null);

            {
                let peers = shared.peers.read().await;
                if let Some(peer) = peers.get(&peer_id) {
                    if !shared.auth_token.is_empty() && !peer.authenticated {
                        let err = encode_frame(
                            "error",
                            json!({ "code": "must_authenticate" }),
                            &shared.module_name,
                        );
                        let _ = peer.tx.send(err);
                        return;
                    }
                }
            }

            let mut peers = shared.peers.write().await;
            if let Some(peer) = peers.get_mut(&peer_id) {
                peer.name = Some(name.clone());
                peer.identity = identity.clone();
            }

            let announce_frame = encode_frame(
                "module:announced",
                json!({ "name": name, "identity": identity }),
                &shared.module_name,
            );
            for (pid, peer) in peers.iter() {
                if *pid != peer_id && peer.authenticated {
                    let _ = peer.tx.send(announce_frame.clone());
                }
            }
            drop(peers);

            let _ = shared.bevy_tx.send(Inbound::Envelope(envelope));
        }

        "transport:connection:heartbeat" => {
            let kind_field = envelope
                .data
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("ping");
            if kind_field == "ping" {
                let peers = shared.peers.read().await;
                if let Some(peer) = peers.get(&peer_id) {
                    let pong = encode_frame(
                        "transport:connection:heartbeat",
                        json!({ "kind": "pong", "timestamp": now_ms() }),
                        &shared.module_name,
                    );
                    let _ = peer.tx.send(pong);
                }
            }
        }

        _ => {
            // Require auth to publish anything else when a token is configured.
            if !shared.auth_token.is_empty() {
                let peers = shared.peers.read().await;
                if !peers.get(&peer_id).map(|p| p.authenticated).unwrap_or(false) {
                    return;
                }
            }

            // Re-serialize the envelope so peers always see a consistent raw shape.
            let frame = match serde_json::to_string(&envelope) {
                Ok(s) => s,
                Err(_) => return,
            };
            broadcast_to_all(shared, frame, Some(peer_id)).await;
            let _ = shared.inbound_bcast.send(envelope.clone());
            let _ = shared.bevy_tx.send(Inbound::Envelope(envelope));
        }
    }
}

async fn broadcast_to_all(shared: &HubShared, frame: String, exclude: Option<Uuid>) {
    let peers = shared.peers.read().await;
    for (pid, peer) in peers.iter() {
        if Some(*pid) != exclude && peer.authenticated {
            let _ = peer.tx.send(frame.clone());
        }
    }
}

fn encode_frame(kind: &str, data: Value, module_name: &str) -> String {
    encode_frame_with_id(kind, data, module_name, None)
}

fn encode_frame_with_id(
    kind: &str,
    data: Value,
    module_name: &str,
    event_id: Option<String>,
) -> String {
    let id = event_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let body = json!({
        "type": kind,
        "data": data,
        "metadata": {
            "event": { "id": id },
            "source": { "kind": "module", "id": module_name },
        }
    });
    body.to_string()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ---------- Bevy-side pump -----------------------------------------------------

fn pump_hub_into_bevy(
    inbound: Option<Res<HubInbound>>,
    mut state: ResMut<HubState>,
    mut chat: MessageWriter<ChatCompleteMessage>,
    mut look: MessageWriter<LookAtRequestMessage>,
    mut tts: MessageWriter<TtsSpeakMessage>,
    mut input_text: MessageWriter<HubInputTextMessage>,
    mut raw: MessageWriter<WsIncomingMessage>,
) {
    let Some(inbound) = inbound else {
        return;
    };
    while let Ok(ev) = inbound.rx.try_recv() {
        match ev {
            Inbound::PeerCount(n) => state.peer_count = n,
            Inbound::Envelope(body) => {
                match body.message_type.as_str() {
                    "output:gen-ai:chat:complete" => {
                        if let Some(content) = extract_chat_content(&body.data) {
                            let speak = strip_act_delay_for_tts(&content).to_string();
                            chat.write(ChatCompleteMessage { content });
                            if !speak.is_empty() {
                                tts.write(TtsSpeakMessage { text: speak });
                            }
                        }
                    }
                    "vrm:set-look-at" => {
                        let local_target = parse_look_at(&body.data);
                        look.write(LookAtRequestMessage { local_target });
                    }
                    "input:text" => {
                        if let Some(text) = body
                            .data
                            .get("text")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                        {
                            let source = body
                                .data
                                .get("source")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            input_text.write(HubInputTextMessage { text, source });
                        }
                    }
                    _ => {}
                }
                raw.write(WsIncomingMessage { envelope: body });
            }
        }
    }
}

fn extract_chat_content(data: &Value) -> Option<String> {
    data.get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            data.get("content")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
        })
}

fn parse_look_at(v: &Value) -> Option<Vec3> {
    let x = v.get("x").and_then(Value::as_f64)?;
    let y = v.get("y").and_then(Value::as_f64)?;
    let z = v.get("z").and_then(Value::as_f64)?;
    Some(Vec3::new(x as f32, y as f32, z as f32))
}
