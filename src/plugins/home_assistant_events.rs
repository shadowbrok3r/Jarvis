//! Home Assistant WebSocket subscription for `state_changed` events with `airi_` prefix.
//!
//! We keep a small ring of recent changes and inject them into the next outbound
//! AiRi turn so the assistant can react to HA state transitions without polling.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::{Value, json};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

use jarvis_avatar::config::Settings;

use super::shared_runtime::SharedTokio;

const AIRI_PREFIX: &str = "airi_";
const MAX_QUEUE: usize = 64;
const MAX_EVENTS_PER_TURN: usize = 12;

#[derive(Debug, Clone)]
pub struct AiriHaStateEvent {
    pub entity_id: String,
    pub old_state: Option<String>,
    pub new_state: Option<String>,
    pub changed_at_ms: u128,
}

#[derive(Resource, Default)]
pub struct AiriHaEventQueue {
    pending: VecDeque<AiriHaStateEvent>,
}

impl AiriHaEventQueue {
    pub fn push(&mut self, ev: AiriHaStateEvent) {
        self.pending.push_back(ev);
        while self.pending.len() > MAX_QUEUE {
            self.pending.pop_front();
        }
    }

    pub fn take_context_block(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let take = self.pending.len().min(MAX_EVENTS_PER_TURN);
        let start = self.pending.len().saturating_sub(take);
        let mut lines = Vec::with_capacity(take + 2);
        lines
            .push("Home Assistant state_changed events (entity_id starts with airi_):".to_string());
        for ev in self.pending.drain(start..) {
            let old = ev.old_state.unwrap_or_else(|| "null".to_string());
            let new = ev.new_state.unwrap_or_else(|| "null".to_string());
            lines.push(format!(
                "- {}: {} -> {} @ {}",
                ev.entity_id, old, new, ev.changed_at_ms
            ));
        }
        Some(lines.join("\n"))
    }
}

#[derive(Resource)]
struct HaWsAiriBridge {
    tx: Sender<AiriHaStateEvent>,
    rx: Receiver<AiriHaStateEvent>,
}

#[derive(Resource, Default)]
struct HaWsAiriRuntime {
    started: bool,
}

pub struct HomeAssistantEventsPlugin;

impl Plugin for HomeAssistantEventsPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = unbounded();
        app.insert_resource(HaWsAiriBridge { tx, rx })
            .init_resource::<HaWsAiriRuntime>()
            .init_resource::<AiriHaEventQueue>()
            .add_systems(Update, (start_ha_airi_ws_task, pump_ha_airi_ws_events));
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn websocket_url_from_ha_url(ha_url: &str) -> Result<String, String> {
    let mut url = Url::parse(ha_url.trim()).map_err(|e| format!("ha_url parse: {e}"))?;
    match url.scheme() {
        "http" => url
            .set_scheme("ws")
            .map_err(|_| "failed to switch HA URL to ws scheme".to_string())?,
        "https" => url
            .set_scheme("wss")
            .map_err(|_| "failed to switch HA URL to wss scheme".to_string())?,
        "ws" | "wss" => {}
        s => return Err(format!("unsupported HA URL scheme for websocket: {s}")),
    }
    url.set_path("/api/websocket");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

async fn read_json_message(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<Value, String> {
    loop {
        let msg = ws
            .next()
            .await
            .ok_or_else(|| "HA websocket closed".to_string())?
            .map_err(|e| format!("HA websocket recv: {e}"))?;
        match msg {
            WsMessage::Text(txt) => {
                return serde_json::from_str::<Value>(&txt)
                    .map_err(|e| format!("HA websocket JSON: {e}"));
            }
            WsMessage::Binary(_) => continue,
            WsMessage::Ping(payload) => {
                ws.send(WsMessage::Pong(payload))
                    .await
                    .map_err(|e| format!("HA websocket pong send: {e}"))?;
            }
            WsMessage::Pong(_) => continue,
            WsMessage::Close(_) => return Err("HA websocket closed".to_string()),
            _ => continue,
        }
    }
}

fn state_from_event_side(v: &Value) -> Option<String> {
    if v.is_null() {
        None
    } else if let Some(s) = v.get("state").and_then(Value::as_str) {
        Some(s.to_string())
    } else if let Some(s) = v.as_str() {
        Some(s.to_string())
    } else {
        None
    }
}

fn parse_airi_state_changed(v: &Value) -> Option<AiriHaStateEvent> {
    if v.get("type").and_then(Value::as_str) != Some("event") {
        return None;
    }
    if v.get("event")
        .and_then(|e| e.get("event_type"))
        .and_then(Value::as_str)
        != Some("state_changed")
    {
        return None;
    }
    let data = v.get("event")?.get("data")?;
    let entity_id = data.get("entity_id")?.as_str()?.to_string();
    if !entity_id.starts_with(AIRI_PREFIX) {
        return None;
    }
    Some(AiriHaStateEvent {
        entity_id,
        old_state: state_from_event_side(data.get("old_state").unwrap_or(&Value::Null)),
        new_state: state_from_event_side(data.get("new_state").unwrap_or(&Value::Null)),
        changed_at_ms: now_ms(),
    })
}

async fn run_ha_airi_event_loop(ws_url: String, token: String, tx: Sender<AiriHaStateEvent>) {
    loop {
        let connected = connect_async(&ws_url).await;
        let (mut ws, _) = match connected {
            Ok(ok) => ok,
            Err(e) => {
                warn!(target: "home_assistant", "HA ws connect failed: {e}");
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        // 1) Expect auth_required
        match read_json_message(&mut ws).await {
            Ok(v) if v.get("type").and_then(Value::as_str) == Some("auth_required") => {}
            Ok(v) => {
                warn!(target: "home_assistant", "HA ws unexpected pre-auth frame: {v}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(e) => {
                warn!(target: "home_assistant", "HA ws auth_required read failed: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        }

        // 2) Send auth
        if let Err(e) = ws
            .send(WsMessage::Text(
                json!({
                    "type": "auth",
                    "access_token": token
                })
                .to_string()
                .into(),
            ))
            .await
        {
            warn!(target: "home_assistant", "HA ws auth send failed: {e}");
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        match read_json_message(&mut ws).await {
            Ok(v) if v.get("type").and_then(Value::as_str) == Some("auth_ok") => {}
            Ok(v) => {
                warn!(target: "home_assistant", "HA ws auth rejected/unexpected: {v}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Err(e) => {
                warn!(target: "home_assistant", "HA ws auth result read failed: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        }

        // 3) Subscribe to state_changed
        if let Err(e) = ws
            .send(WsMessage::Text(
                json!({
                    "id": 1,
                    "type": "subscribe_events",
                    "event_type": "state_changed"
                })
                .to_string()
                .into(),
            ))
            .await
        {
            warn!(target: "home_assistant", "HA ws subscribe send failed: {e}");
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        // 4) Wait for subscribe result
        match read_json_message(&mut ws).await {
            Ok(v)
                if v.get("type").and_then(Value::as_str) == Some("result")
                    && v.get("success").and_then(Value::as_bool) == Some(true) => {}
            Ok(v) => {
                warn!(target: "home_assistant", "HA ws subscribe failed/unexpected: {v}");
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
            Err(e) => {
                warn!(target: "home_assistant", "HA ws subscribe result read failed: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        }

        info!(target: "home_assistant", "HA ws subscribed to state_changed for airi_* forwarding");

        // 5) Event stream
        loop {
            match read_json_message(&mut ws).await {
                Ok(v) => {
                    if let Some(ev) = parse_airi_state_changed(&v) {
                        let _ = tx.send(ev);
                    }
                }
                Err(e) => {
                    warn!(target: "home_assistant", "HA ws event loop ended: {e}");
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn start_ha_airi_ws_task(
    mut rt: ResMut<HaWsAiriRuntime>,
    settings: Res<Settings>,
    bridge: Res<HaWsAiriBridge>,
    tokio_rt: Option<Res<SharedTokio>>,
) {
    if rt.started {
        return;
    }
    let Some(tokio_rt) = tokio_rt else {
        return;
    };
    let ha = &settings.home_assistant;
    if ha.ha_url.trim().is_empty() || ha.ha_token.trim().is_empty() {
        return;
    }
    let ws_url = match websocket_url_from_ha_url(ha.ha_url.trim()) {
        Ok(u) => u,
        Err(e) => {
            warn!(target: "home_assistant", "HA ws disabled: {e}");
            rt.started = true;
            return;
        }
    };
    let token = ha.ha_token.trim().to_string();
    let tx = bridge.tx.clone();
    tokio_rt.spawn(async move {
        run_ha_airi_event_loop(ws_url, token, tx).await;
    });
    rt.started = true;
}

fn pump_ha_airi_ws_events(bridge: Res<HaWsAiriBridge>, mut queue: ResMut<AiriHaEventQueue>) {
    loop {
        match bridge.rx.try_recv() {
            Ok(ev) => queue.push(ev),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
}
