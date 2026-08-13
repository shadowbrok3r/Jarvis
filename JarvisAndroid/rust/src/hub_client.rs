//! WebSocket client to the desktop channel hub.
//!
//! The desktop *hosts* the hub (`ChannelHubPlugin`, an axum listener); a phone
//! is a peer, so this is the mirror image: connect to `ws://<host>/ws`, send
//! `module:authenticate`, then republish inbound frames as
//! `WsIncomingMessage` — the same message `hub_pose_apply`, `expressions`, and
//! `look_at` already read on desktop.
//!
//! Enabled by `settings.ironclaw.client_url`; empty (the desktop default) keeps
//! the whole plugin inert.

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use futures_util::{SinkExt, StreamExt};
use jarvis_avatar::config::Settings;
use jarvis_avatar::ironclaw::protocol::EnvelopeBody;
use jarvis_avatar::plugins::channel_server::{HubBroadcast, HubOutbox, WsIncomingMessage};
use jarvis_avatar::plugins::shared_runtime::SharedTokio;
use serde_json::{Value, json};

/// Seconds between reconnect attempts. A phone roams networks and sleeps, so a
/// dropped socket is the normal case, not an error.
const RECONNECT_SECS: u64 = 5;

pub struct HubClientPlugin;

impl Plugin for HubClientPlugin {
    fn build(&self, app: &mut App) {
        // Must exist before PostStartup: `ironclaw_chat` and `zeroclaw_chat` take
        // `Res<HubBroadcast>` un-optioned, and only the axum server creates one.
        // A phone is a peer, so it gets a detached handle whose queue this
        // plugin forwards over the WebSocket.
        let module = app
            .world()
            .resource::<Settings>()
            .ironclaw
            .module_name
            .clone();
        let (hub, outbox) = HubBroadcast::detached(module);
        app.insert_resource(hub)
            .insert_resource(outbox)
            .add_systems(PostStartup, spawn_hub_client)
            .add_systems(Update, (pump_hub_into_bevy, pump_outbox_to_hub));
    }
}

#[derive(Resource)]
struct HubInbox {
    rx: Receiver<EnvelopeBody>,
}

/// Wire-ready JSON handed to the socket task.
#[derive(Resource)]
struct HubEgress {
    tx: Sender<String>,
}

/// Live connection state, for the overlay's status readout.
#[derive(Resource, Default)]
pub struct HubLink {
    pub connected: bool,
    pub url: String,
    pub frames: u64,
}

fn spawn_hub_client(mut commands: Commands, settings: Res<Settings>, rt: Option<Res<SharedTokio>>) {
    let raw = settings.ironclaw.client_url.trim().to_string();
    if raw.is_empty() {
        info!("hub client disabled (ironclaw.client_url empty)");
        return;
    }
    let Some(rt) = rt else {
        error!("hub client needs SharedRuntimePlugin");
        return;
    };

    let ws_url = to_ws_url(&raw);
    let token = settings.ironclaw.auth_token.clone();
    let module = settings.ironclaw.module_name.clone();
    let (tx, rx) = unbounded();
    let (egress_tx, egress_rx) = unbounded::<String>();

    commands.insert_resource(HubInbox { rx });
    commands.insert_resource(HubEgress { tx: egress_tx });
    commands.insert_resource(HubLink {
        connected: false,
        url: ws_url.clone(),
        frames: 0,
    });

    info!("hub client -> {ws_url}");
    rt.spawn(async move {
        loop {
            if let Err(e) = run_session(&ws_url, &token, &module, &tx, &egress_rx).await {
                warn!("hub client: {e}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(RECONNECT_SECS)).await;
        }
    });
}

/// `http://host:6121` / `host:6121` / `ws://host:6121` → `ws://host:6121/ws`.
fn to_ws_url(raw: &str) -> String {
    let trimmed = raw.trim_end_matches('/');
    let base = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_string()
    } else {
        format!("ws://{trimmed}")
    };
    if base.ends_with("/ws") {
        base
    } else {
        format!("{base}/ws")
    }
}

async fn run_session(
    url: &str,
    token: &str,
    module: &str,
    tx: &Sender<EnvelopeBody>,
    egress: &Receiver<String>,
) -> Result<(), String> {
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("connect {url}: {e}"))?;

    // The hub rejects publish/receive until this lands whenever its token is set.
    let auth = frame("module:authenticate", json!({ "token": token }), module);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(auth.into()))
        .await
        .map_err(|e| format!("authenticate: {e}"))?;
    let announce = frame("module:announce", json!({ "name": module }), module);
    let _ = socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            announce.into(),
        ))
        .await;

    loop {
        // The egress queue is filled by a Bevy system, so it is polled rather
        // than awaited; 20 ms keeps chat sends responsive without busy-looping.
        tokio::select! {
            biased;
            incoming = socket.next() => {
                let Some(msg) = incoming else { break };
                match msg.map_err(|e| format!("read: {e}"))? {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        if let Some(env) = parse_envelope(&text) {
                            let _ = tx.send(env);
                        }
                    }
                    tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                        if let Ok(text) = std::str::from_utf8(&bytes)
                            && let Some(env) = parse_envelope(text)
                        {
                            let _ = tx.send(env);
                        }
                    }
                    tokio_tungstenite::tungstenite::Message::Close(_) => break,
                    _ => {}
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {
                while let Ok(json) = egress.try_recv() {
                    socket
                        .send(tokio_tungstenite::tungstenite::Message::Text(json.into()))
                        .await
                        .map_err(|e| format!("send: {e}"))?;
                }
            }
        }
    }
    Err("socket closed".into())
}

/// Accepts raw `{type,data,metadata}` and superjson `{json:{…},meta:{}}` alike,
/// matching the hub's own `handle_peer_text`.
fn parse_envelope(text: &str) -> Option<EnvelopeBody> {
    let value: Value = serde_json::from_str(text).ok()?;
    let body = value.get("json").cloned().unwrap_or(value);
    serde_json::from_value(body).ok()
}

fn frame(kind: &str, data: Value, module: &str) -> String {
    json!({
        "type": kind,
        "data": data,
        "metadata": {
            "event": { "id": uuid_v4() },
            "source": { "kind": "module", "id": module },
        }
    })
    .to_string()
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Moves frames the chat plugins published through `HubBroadcast` onto the socket.
fn pump_outbox_to_hub(outbox: Option<Res<HubOutbox>>, egress: Option<Res<HubEgress>>) {
    let (Some(outbox), Some(egress)) = (outbox, egress) else {
        return;
    };
    while let Some(json) = outbox.try_recv_json() {
        let _ = egress.tx.send(json);
    }
}

fn pump_hub_into_bevy(
    inbox: Option<Res<HubInbox>>,
    mut link: Option<ResMut<HubLink>>,
    mut out: MessageWriter<WsIncomingMessage>,
) {
    let Some(inbox) = inbox else { return };
    let mut n = 0u64;
    for envelope in inbox.rx.try_iter() {
        out.write(WsIncomingMessage { envelope });
        n += 1;
    }
    if n > 0 && let Some(link) = link.as_mut() {
        link.connected = true;
        link.frames += n;
    }
}
