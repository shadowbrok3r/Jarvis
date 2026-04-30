//! Ring-buffered network / protocol trace for the debug "Network trace" window.
//!
//! Tokio and std threads push via [`TrafficLogSink`]; the UI reads the same `Arc`
//! under a short mutex lock. Sensitive substrings are redacted in summaries.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use serde_json::Value;

const MAX_PER_CHANNEL: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrafficDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrafficChannel {
    ChannelHubWsInbound,
    ChannelHubWsOutbound,
    ChannelHubHttpBroadcast,
    IronclawGatewaySse,
    IronclawGatewayHttp,
    TtsHttp,
    McpHttp,
    HomeAssistantHttp,
    A2fGrpc,
}

impl TrafficChannel {
    pub const ALL: &[TrafficChannel] = &[
        TrafficChannel::ChannelHubWsInbound,
        TrafficChannel::ChannelHubWsOutbound,
        TrafficChannel::ChannelHubHttpBroadcast,
        TrafficChannel::IronclawGatewaySse,
        TrafficChannel::IronclawGatewayHttp,
        TrafficChannel::TtsHttp,
        TrafficChannel::McpHttp,
        TrafficChannel::HomeAssistantHttp,
        TrafficChannel::A2fGrpc,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TrafficChannel::ChannelHubWsInbound => "Hub · WS inbound",
            TrafficChannel::ChannelHubWsOutbound => "Hub · WS outbound",
            TrafficChannel::ChannelHubHttpBroadcast => "Hub · HTTP /broadcast",
            TrafficChannel::IronclawGatewaySse => "Gateway · SSE",
            TrafficChannel::IronclawGatewayHttp => "Gateway · HTTP",
            TrafficChannel::TtsHttp => "TTS · HTTP",
            TrafficChannel::McpHttp => "MCP · HTTP",
            TrafficChannel::HomeAssistantHttp => "Home Assistant · HTTP",
            TrafficChannel::A2fGrpc => "A2F · gRPC",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrafficEntry {
    pub unix_ms: u128,
    pub direction: TrafficDirection,
    pub summary: String,
    pub payload: Option<Value>,
}

#[derive(Default)]
pub struct TrafficStore {
    paused: bool,
    buffers: HashMap<TrafficChannel, VecDeque<TrafficEntry>>,
}

impl TrafficStore {
    pub fn push(
        &mut self,
        ch: TrafficChannel,
        direction: TrafficDirection,
        summary: impl Into<String>,
        payload: Option<Value>,
    ) {
        if self.paused {
            return;
        }
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let entry = TrafficEntry {
            unix_ms,
            direction,
            summary: redact_secrets(summary.into()),
            payload: payload.map(redact_json_value),
        };
        let dq = self
            .buffers
            .entry(ch)
            .or_insert_with(|| VecDeque::with_capacity(MAX_PER_CHANNEL + 1));
        dq.push_back(entry);
        while dq.len() > MAX_PER_CHANNEL {
            dq.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.buffers.clear();
    }

    pub fn clear_channel(&mut self, ch: TrafficChannel) {
        self.buffers.remove(&ch);
    }

    pub fn set_paused(&mut self, p: bool) {
        self.paused = p;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn entries(&self, ch: TrafficChannel) -> Vec<TrafficEntry> {
        self.buffers
            .get(&ch)
            .map(|d| d.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Redact obvious bearer / HA token fragments from free-text logs.
pub fn redact_secrets(mut s: String) -> String {
    const MARK: &str = "Bearer ";
    while let Some(i) = s.find(MARK) {
        let start = i + MARK.len();
        let mut end = start;
        for (idx, c) in s[start..].char_indices() {
            if c.is_whitespace() || c == '"' || c == ',' || c == '}' {
                break;
            }
            end = start + idx + c.len_utf8();
            if end - start > 160 {
                break;
            }
        }
        if end > start {
            s.replace_range(start..end, "[REDACTED]");
        } else {
            break;
        }
    }
    s
}

fn redact_json_value(mut v: Value) -> Value {
    match v {
        Value::Object(ref mut m) => {
            for (k, val) in m.iter_mut() {
                let kl = k.to_ascii_lowercase();
                if kl.contains("token") || kl == "authorization" || kl == "ha_token" {
                    *val = Value::String("[REDACTED]".into());
                } else {
                    *val = redact_json_value(val.clone());
                }
            }
        }
        Value::Array(ref mut a) => {
            for el in a.iter_mut() {
                *el = redact_json_value(el.clone());
            }
        }
        _ => {}
    }
    v
}

#[derive(Resource, Clone)]
pub struct TrafficLogSink(pub Arc<Mutex<TrafficStore>>);

impl TrafficLogSink {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(TrafficStore::default())))
    }

    pub fn push(
        &self,
        ch: TrafficChannel,
        direction: TrafficDirection,
        summary: impl Into<String>,
        payload: Option<Value>,
    ) {
        if let Ok(mut g) = self.0.lock() {
            g.push(ch, direction, summary, payload);
        }
    }

    pub fn snapshot_channel(&self, ch: TrafficChannel) -> Vec<TrafficEntry> {
        self.0.lock().map(|g| g.entries(ch)).unwrap_or_default()
    }

    pub fn clear_all(&self) {
        if let Ok(mut g) = self.0.lock() {
            g.clear();
        }
    }

    pub fn clear_one(&self, ch: TrafficChannel) {
        if let Ok(mut g) = self.0.lock() {
            g.clear_channel(ch);
        }
    }

    pub fn set_paused(&self, p: bool) {
        if let Ok(mut g) = self.0.lock() {
            g.set_paused(p);
        }
    }

    pub fn is_paused(&self) -> bool {
        self.0.lock().map(|g| g.is_paused()).unwrap_or(false)
    }
}

impl Default for TrafficLogSink {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TrafficLogPlugin;

impl Plugin for TrafficLogPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(TrafficLogSink::new());
    }
}
