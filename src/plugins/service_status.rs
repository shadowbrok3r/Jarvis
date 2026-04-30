//! Unified connection / liveness tracking for every external service the
//! avatar talks to. The debug UI's "Services" panel reads this one resource;
//! each owning plugin (hub, gateway, TTS, Kimodo peer, A2F, MCP) posts
//! updates here as its state changes.
//!
//! States are deliberately coarse:
//!   * `Disabled`   — turned off in config; we don't probe it.
//!   * `Unknown`    — no status yet (startup race).
//!   * `Connecting` — probe in flight / waiting for first reply.
//!   * `Online`     — last probe / event indicated the service is healthy.
//!   * `Offline`    — last attempt failed. `detail` carries the reason.

use std::time::{Duration, Instant};

use bevy::prelude::*;
use reqwest::Client;
use serde_json::Value;
use tokio::task::JoinHandle;

use jarvis_avatar::config::Settings;
use jarvis_avatar::ironclaw::protocol::EnvelopeBody;

use super::channel_server::{HubBroadcast, HubState, WsIncomingMessage};
use super::ironclaw_chat::{ChatState, ChatStatusMessage};
use super::shared_runtime::SharedTokio;

/// Stable identifier for each service we track. Ordered for UI display.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ServiceId {
    ChannelHub,
    IronclawGateway,
    IronclawPeer,
    KimodoPeer,
    HaVoiceBridge,
    Mcp,
    A2fGrpc,
    A2fHealth,
    Tts,
}

impl ServiceId {
    pub const ALL: &'static [ServiceId] = &[
        ServiceId::ChannelHub,
        ServiceId::IronclawGateway,
        ServiceId::IronclawPeer,
        ServiceId::KimodoPeer,
        ServiceId::HaVoiceBridge,
        ServiceId::Mcp,
        ServiceId::A2fGrpc,
        ServiceId::A2fHealth,
        ServiceId::Tts,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ServiceId::ChannelHub => "Channel Hub (WS :6121)",
            ServiceId::IronclawGateway => "IronClaw Gateway (SSE :3000)",
            ServiceId::IronclawPeer => "IronClaw Peer (hub WS)",
            ServiceId::KimodoPeer => "Kimodo Peer (hub WS)",
            ServiceId::HaVoiceBridge => "HA Voice Bridge (hub WS)",
            ServiceId::Mcp => "MCP Streamable HTTP",
            ServiceId::A2fGrpc => "Audio2Face gRPC",
            ServiceId::A2fHealth => "Audio2Face HTTP /health",
            ServiceId::Tts => "TTS (Kokoro)",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ServiceState {
    Disabled,
    Unknown,
    Connecting,
    Online,
    Offline,
}

impl ServiceState {
    pub fn color(self) -> bevy_egui::egui::Color32 {
        use bevy_egui::egui::Color32;
        match self {
            ServiceState::Online => Color32::from_rgb(80, 200, 120),
            ServiceState::Connecting => Color32::from_rgb(240, 200, 80),
            ServiceState::Offline => Color32::from_rgb(235, 85, 100),
            ServiceState::Unknown => Color32::from_rgb(150, 150, 160),
            ServiceState::Disabled => Color32::from_rgb(95, 95, 105),
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            ServiceState::Online => "online",
            ServiceState::Connecting => "connecting",
            ServiceState::Offline => "offline",
            ServiceState::Unknown => "unknown",
            ServiceState::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServiceEntry {
    pub state: ServiceState,
    pub endpoint: String,
    pub detail: String,
    pub last_change: Option<Instant>,
    pub last_ok: Option<Instant>,
}

impl Default for ServiceEntry {
    fn default() -> Self {
        Self {
            state: ServiceState::Unknown,
            endpoint: String::new(),
            detail: String::new(),
            last_change: None,
            last_ok: None,
        }
    }
}

#[derive(Resource, Default)]
pub struct ServiceStatus {
    pub entries: std::collections::HashMap<ServiceId, ServiceEntry>,
}

impl ServiceStatus {
    pub fn set(
        &mut self,
        id: ServiceId,
        state: ServiceState,
        endpoint: impl Into<String>,
        detail: impl Into<String>,
    ) {
        let endpoint = endpoint.into();
        let detail = detail.into();
        let entry = self.entries.entry(id).or_default();
        let changed = entry.state != state || !endpoint.is_empty() && entry.endpoint != endpoint;
        if !endpoint.is_empty() {
            entry.endpoint = endpoint;
        }
        entry.detail = detail;
        if changed {
            entry.state = state;
            entry.last_change = Some(Instant::now());
        } else {
            entry.state = state;
        }
        if matches!(state, ServiceState::Online) {
            entry.last_ok = Some(Instant::now());
        }
    }

    pub fn get(&self, id: ServiceId) -> Option<&ServiceEntry> {
        self.entries.get(&id)
    }
}

// Internal bookkeeping for the HTTP health prober so we don't pile up
// concurrent requests when the Update tick is fast.
#[derive(Resource, Default)]
struct ProbeTimers {
    last_probe: Option<Instant>,
    in_flight: Option<JoinHandle<()>>,
    result_rx: Option<crossbeam_channel::Receiver<ProbeBatch>>,
}

#[derive(Debug, Clone)]
struct ProbeBatch {
    a2f_health: Option<ServiceUpdate>,
    a2f_grpc: Option<ServiceUpdate>,
    tts: Option<ServiceUpdate>,
    mcp: Option<ServiceUpdate>,
    gateway: Option<ServiceUpdate>,
}

#[derive(Debug, Clone)]
struct ServiceUpdate {
    state: ServiceState,
    endpoint: String,
    detail: String,
}

pub struct ServiceStatusPlugin;

impl Plugin for ServiceStatusPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ServiceStatus>()
            .init_resource::<ProbeTimers>()
            .add_systems(Startup, seed_initial_states)
            .add_systems(
                Update,
                (
                    apply_hub_state,
                    apply_hub_peer_names,
                    apply_gateway_state,
                    run_http_probes,
                    apply_probe_results,
                ),
            );
    }
}

fn seed_initial_states(mut status: ResMut<ServiceStatus>, settings: Res<Settings>) {
    let s = &settings;

    status.set(
        ServiceId::ChannelHub,
        ServiceState::Connecting,
        &s.ironclaw.bind_address,
        "waiting for hub to bind",
    );

    status.set(
        ServiceId::IronclawGateway,
        ServiceState::Connecting,
        &s.gateway.base_url,
        "contacting gateway",
    );

    status.set(
        ServiceId::IronclawPeer,
        ServiceState::Unknown,
        "",
        "waiting for ironclaw module to connect to hub",
    );
    status.set(
        ServiceId::KimodoPeer,
        ServiceState::Unknown,
        "",
        "waiting for kimodo peer to connect to hub",
    );
    status.set(
        ServiceId::HaVoiceBridge,
        ServiceState::Unknown,
        "",
        "waiting for ha-voice-bridge to connect to hub",
    );

    if s.mcp.enabled {
        status.set(
            ServiceId::Mcp,
            ServiceState::Connecting,
            format!("http://{}{}", s.mcp.bind_address, s.mcp.path),
            "starting MCP listener",
        );
    } else {
        status.set(
            ServiceId::Mcp,
            ServiceState::Disabled,
            &s.mcp.bind_address,
            "mcp disabled in config",
        );
    }

    if s.a2f.enabled {
        status.set(
            ServiceId::A2fGrpc,
            ServiceState::Connecting,
            &s.a2f.endpoint,
            "probing gRPC endpoint",
        );
        status.set(
            ServiceId::A2fHealth,
            ServiceState::Connecting,
            &s.a2f.health_url,
            "probing health endpoint",
        );
    } else {
        status.set(
            ServiceId::A2fGrpc,
            ServiceState::Disabled,
            &s.a2f.endpoint,
            "a2f disabled in config",
        );
        status.set(
            ServiceId::A2fHealth,
            ServiceState::Disabled,
            &s.a2f.health_url,
            "a2f disabled in config",
        );
    }

    if s.tts.enabled {
        status.set(
            ServiceId::Tts,
            ServiceState::Connecting,
            &s.tts.kokoro_url,
            "probing /v1/models",
        );
    } else {
        status.set(
            ServiceId::Tts,
            ServiceState::Disabled,
            &s.tts.kokoro_url,
            "tts disabled in config",
        );
    }
}

// ----- Hub-derived state -----------------------------------------------------

fn apply_hub_state(hub: Option<Res<HubState>>, mut status: ResMut<ServiceStatus>) {
    let Some(hub) = hub else { return };
    if !hub.is_changed()
        && status.get(ServiceId::ChannelHub).map(|e| e.state) != Some(ServiceState::Connecting)
    {
        return;
    }
    let endpoint = hub.bound_to.clone().unwrap_or_default();
    let state = if hub.bound_to.is_some() {
        ServiceState::Online
    } else {
        ServiceState::Offline
    };
    status.set(
        ServiceId::ChannelHub,
        state,
        endpoint,
        format!("{} peer(s) connected", hub.peer_count),
    );
}

/// Classify hub peers by announced module name so the Services panel can
/// surface per-peer connectivity (Kimodo, IronClaw, HA voice bridge).
fn apply_hub_peer_names(
    mut incoming: MessageReader<WsIncomingMessage>,
    mut status: ResMut<ServiceStatus>,
) {
    for msg in incoming.read() {
        if msg.envelope.message_type != "module:announce" {
            continue;
        }
        let Some(name) = peer_name(&msg.envelope) else {
            continue;
        };
        let n = name.to_ascii_lowercase();
        let detail = format!("announced '{name}'");
        if n.contains("kimodo") {
            status.set(
                ServiceId::KimodoPeer,
                ServiceState::Online,
                name.clone(),
                detail,
            );
        } else if n.contains("ironclaw") || n.contains("proxy") {
            status.set(
                ServiceId::IronclawPeer,
                ServiceState::Online,
                name.clone(),
                detail,
            );
        } else if n.contains("ha-voice") || n.contains("voice-bridge") || n.contains("ha_voice") {
            status.set(
                ServiceId::HaVoiceBridge,
                ServiceState::Online,
                name.clone(),
                detail,
            );
        }
    }
}

fn peer_name(env: &EnvelopeBody) -> Option<String> {
    if let Some(s) = env.data.get("name").and_then(Value::as_str) {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    env.metadata
        .get("source")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

// ----- Gateway-derived state -------------------------------------------------

fn apply_gateway_state(
    chat: Option<Res<ChatState>>,
    mut reader: MessageReader<ChatStatusMessage>,
    mut status: ResMut<ServiceStatus>,
) {
    let Some(chat) = chat else { return };
    let had_events = reader.read().count() > 0;
    if !had_events && !chat.is_changed() {
        return;
    }

    // Derive a state from last_status / last_error. Treat "sse: open" or any
    // "connected" status as Online, transient reconnect messages as
    // Connecting, and anything with an active last_error as Offline.
    let state = if let Some(err) = chat.last_error.as_deref() {
        if chat
            .last_status
            .as_deref()
            .is_some_and(|s| s.contains("open") || s.contains("connected"))
        {
            // SSE reconnected after a prior error.
            ServiceState::Online
        } else {
            let _ = err;
            ServiceState::Offline
        }
    } else if chat
        .last_status
        .as_deref()
        .is_some_and(|s| s.contains("open") || s.contains("connected"))
    {
        ServiceState::Online
    } else {
        ServiceState::Connecting
    };

    let detail = match (&chat.last_status, &chat.last_error) {
        (_, Some(err)) => err.clone(),
        (Some(s), _) => s.clone(),
        _ => String::new(),
    };
    status.set(
        ServiceId::IronclawGateway,
        state,
        chat.base_url.clone(),
        detail,
    );
}

// ----- HTTP probes (A2F health, TTS, MCP, Gateway) ---------------------------

const PROBE_INTERVAL: Duration = Duration::from_secs(10);

fn run_http_probes(
    mut timers: ResMut<ProbeTimers>,
    tokio_rt: Option<Res<SharedTokio>>,
    settings: Res<Settings>,
) {
    let Some(tokio_rt) = tokio_rt else { return };

    let now = Instant::now();
    let due = timers
        .last_probe
        .is_none_or(|last| now.duration_since(last) >= PROBE_INTERVAL);
    if !due {
        return;
    }
    // Don't overlap probes; if the last one is still running, wait.
    if timers.in_flight.as_ref().is_some_and(|h| !h.is_finished()) {
        return;
    }
    timers.last_probe = Some(now);

    let (tx, rx) = crossbeam_channel::bounded::<ProbeBatch>(4);
    timers.result_rx = Some(rx);

    let a2f_enabled = settings.a2f.enabled;
    let a2f_endpoint = settings.a2f.endpoint.clone();
    let a2f_health = settings.a2f.health_url.clone();

    let tts_enabled = settings.tts.enabled;
    let tts_url = settings.tts.kokoro_url.clone();

    let mcp_enabled = settings.mcp.enabled;
    let mcp_bind = settings.mcp.bind_address.clone();
    let mcp_path = settings.mcp.path.clone();

    let gateway_url = settings.gateway.base_url.clone();
    let gateway_token = settings.gateway.auth_token.clone();

    let handle = tokio_rt.spawn(async move {
        let client = Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_else(|_| Client::new());

        let a2f_health = if a2f_enabled {
            Some(probe_http_get(&client, &a2f_health, None, "A2F health").await)
        } else {
            None
        };
        let a2f_grpc = if a2f_enabled {
            Some(probe_grpc_tcp(&a2f_endpoint).await)
        } else {
            None
        };
        let tts = if tts_enabled {
            Some(probe_tts(&client, &tts_url).await)
        } else {
            None
        };
        let mcp = if mcp_enabled {
            Some(probe_mcp(&client, &mcp_bind, &mcp_path).await)
        } else {
            None
        };
        let gateway = Some(probe_gateway(&client, &gateway_url, &gateway_token).await);

        let _ = tx.send(ProbeBatch {
            a2f_health,
            a2f_grpc,
            tts,
            mcp,
            gateway,
        });
    });

    timers.in_flight = Some(handle);
}

fn apply_probe_results(timers: Res<ProbeTimers>, mut status: ResMut<ServiceStatus>) {
    let Some(rx) = timers.result_rx.as_ref() else {
        return;
    };
    while let Ok(batch) = rx.try_recv() {
        if let Some(u) = batch.a2f_health {
            status.set(ServiceId::A2fHealth, u.state, u.endpoint, u.detail);
        }
        if let Some(u) = batch.a2f_grpc {
            status.set(ServiceId::A2fGrpc, u.state, u.endpoint, u.detail);
        }
        if let Some(u) = batch.tts {
            status.set(ServiceId::Tts, u.state, u.endpoint, u.detail);
        }
        if let Some(u) = batch.mcp {
            status.set(ServiceId::Mcp, u.state, u.endpoint, u.detail);
        }
        if let Some(u) = batch.gateway {
            status.set(ServiceId::IronclawGateway, u.state, u.endpoint, u.detail);
        }
    }
}

async fn probe_http_get(
    client: &Client,
    url: &str,
    bearer: Option<&str>,
    label: &str,
) -> ServiceUpdate {
    if url.trim().is_empty() {
        return ServiceUpdate {
            state: ServiceState::Unknown,
            endpoint: url.to_string(),
            detail: format!("{label}: no URL configured"),
        };
    }
    let mut req = client.get(url);
    if let Some(b) = bearer {
        if !b.is_empty() {
            req = req.bearer_auth(b);
        }
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                ServiceUpdate {
                    state: ServiceState::Online,
                    endpoint: url.to_string(),
                    detail: format!("{label} HTTP {}", status.as_u16()),
                }
            } else if status.as_u16() == 401 || status.as_u16() == 403 {
                // A reachable endpoint that merely rejected our creds still
                // counts as Online — the service IS up.
                ServiceUpdate {
                    state: ServiceState::Online,
                    endpoint: url.to_string(),
                    detail: format!("{label} HTTP {} (auth required)", status.as_u16()),
                }
            } else {
                ServiceUpdate {
                    state: ServiceState::Offline,
                    endpoint: url.to_string(),
                    detail: format!("{label} HTTP {}", status.as_u16()),
                }
            }
        }
        Err(e) => ServiceUpdate {
            state: ServiceState::Offline,
            endpoint: url.to_string(),
            detail: short_err(e.to_string()),
        },
    }
}

async fn probe_tts(client: &Client, base_url: &str) -> ServiceUpdate {
    if base_url.trim().is_empty() {
        return ServiceUpdate {
            state: ServiceState::Unknown,
            endpoint: base_url.to_string(),
            detail: "no kokoro URL configured".into(),
        };
    }
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    probe_http_get(client, &url, None, "kokoro /v1/models").await
}

async fn probe_mcp(client: &Client, bind: &str, path: &str) -> ServiceUpdate {
    let host = mcp_host(bind);
    let p = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let url = format!("http://{host}{p}");
    // RMCP streamable-http will reject a bare GET with 405/406/400, but the
    // TCP connection succeeding is enough to call it Online.
    match client.get(&url).send().await {
        Ok(resp) => ServiceUpdate {
            state: ServiceState::Online,
            endpoint: url,
            detail: format!("MCP HTTP {}", resp.status().as_u16()),
        },
        Err(e) => ServiceUpdate {
            state: ServiceState::Offline,
            endpoint: url,
            detail: short_err(e.to_string()),
        },
    }
}

fn mcp_host(bind: &str) -> String {
    // Translate 0.0.0.0 / :: to localhost for probing.
    let b = bind.trim();
    if let Some(rest) = b.strip_prefix("0.0.0.0:") {
        format!("127.0.0.1:{rest}")
    } else if let Some(rest) = b.strip_prefix("[::]:") {
        format!("127.0.0.1:{rest}")
    } else {
        b.to_string()
    }
}

async fn probe_gateway(client: &Client, base_url: &str, token: &str) -> ServiceUpdate {
    if base_url.trim().is_empty() {
        return ServiceUpdate {
            state: ServiceState::Unknown,
            endpoint: base_url.to_string(),
            detail: "no gateway URL configured".into(),
        };
    }
    let url = format!("{}/api/health", base_url.trim_end_matches('/'));
    let bearer = if token.is_empty() { None } else { Some(token) };
    let update = probe_http_get(client, &url, bearer, "gateway /api/health").await;
    // If /api/health 404s, fall back to base_url — at least confirms reachability.
    if update.detail.contains("HTTP 404") {
        return probe_http_get(client, base_url, bearer, "gateway root").await;
    }
    update
}

async fn probe_grpc_tcp(endpoint: &str) -> ServiceUpdate {
    // `endpoint` is a tonic URI like `http://host:port` or bare `host:port`.
    let raw = endpoint
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let addr = raw.split('/').next().unwrap_or(raw);
    if addr.is_empty() {
        return ServiceUpdate {
            state: ServiceState::Unknown,
            endpoint: endpoint.to_string(),
            detail: "no a2f endpoint configured".into(),
        };
    }
    match tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(_)) => ServiceUpdate {
            state: ServiceState::Online,
            endpoint: endpoint.to_string(),
            detail: "TCP connect ok".into(),
        },
        Ok(Err(e)) => ServiceUpdate {
            state: ServiceState::Offline,
            endpoint: endpoint.to_string(),
            detail: short_err(e.to_string()),
        },
        Err(_) => ServiceUpdate {
            state: ServiceState::Offline,
            endpoint: endpoint.to_string(),
            detail: "TCP connect timed out (2s)".into(),
        },
    }
}

fn short_err(s: String) -> String {
    s.chars().take(140).collect()
}

// Intentionally unused import guard — keeps us honest about what this module
// actually touches even if a future refactor stops using the hub broadcast.
#[allow(dead_code)]
fn _hub_type_check(_hub: &HubBroadcast) {}
