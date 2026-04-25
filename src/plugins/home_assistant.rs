//! Home Assistant UI resources: discovery results + async completion pump.

use bevy::prelude::*;
use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};

use jarvis_avatar::config::Settings;
use jarvis_avatar::home_assistant::{self, DiscoverySnapshot};

use super::home_assistant_events::HomeAssistantEventsPlugin;
use super::home_assistant_routing::HomeAssistantRoutingPlugin;
use super::shared_runtime::SharedTokio;

#[derive(Resource, Default)]
pub struct HaDiscoveryUiCache {
    pub last: Option<DiscoverySnapshot>,
    pub last_error: Option<String>,
    pub is_refreshing: bool,
    pub last_refresh_at_ms: Option<u128>,
}

#[derive(Resource)]
pub struct HaDiscoverBridge {
    pub tx: Sender<Result<DiscoverySnapshot, String>>,
    rx: Receiver<Result<DiscoverySnapshot, String>>,
}

/// One-shot: when HA URL + token are set, pull device registry after startup (same as Discover).
#[derive(Resource, Default)]
pub struct HaStartupDiscoverFired(pub bool);

fn ha_post_startup_discover(
    mut fired: ResMut<HaStartupDiscoverFired>,
    mut cache: ResMut<HaDiscoveryUiCache>,
    settings: Res<Settings>,
    bridge: Option<Res<HaDiscoverBridge>>,
    tokio: Option<Res<SharedTokio>>,
) {
    if fired.0 {
        return;
    }

    if !home_assistant::configured(&settings.home_assistant) {
        fired.0 = true;
        return;
    }
    let Some(bridge) = bridge else {
        fired.0 = true;
        return;
    };
    let Some(rt) = tokio else {
        tracing::warn!(target: "home_assistant", "startup discover skipped: SharedTokio not ready");
        fired.0 = true;
        return;
    };
    if cache.is_refreshing {
        fired.0 = true;
        return;
    }

    fired.0 = true;
    let tx = bridge.tx.clone();
    let ha = settings.home_assistant.clone();
    cache.is_refreshing = true;
    cache.last_error = None;

    rt.spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Err(format!("reqwest client: {e}")));
                return;
            }
        };
        let res = home_assistant::discover(&client, &ha).await.map_err(|e| e.to_string());
        let _ = tx.send(res);
    });
}

fn pump_ha_discover_results(
    mut cache: ResMut<HaDiscoveryUiCache>,
    bridge: Option<Res<HaDiscoverBridge>>,
) {
    let Some(bridge) = bridge else {
        return;
    };
    loop {
        match bridge.rx.try_recv() {
            Ok(msg) => {
                cache.is_refreshing = false;
                match msg {
                    Ok(snap) => {
                        cache.last = Some(snap);
                        cache.last_error = None;
                        cache.last_refresh_at_ms =
                            Some(jarvis_avatar::home_assistant::discovery_timestamp_ms());
                    }
                    Err(e) => {
                        cache.last_error = Some(e);
                    }
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
}

pub struct HomeAssistantPlugin;

impl Plugin for HomeAssistantPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = unbounded();
        app.insert_resource(HaDiscoverBridge { tx, rx })
            .init_resource::<HaDiscoveryUiCache>()
            .init_resource::<HaStartupDiscoverFired>()
            .add_plugins((HomeAssistantRoutingPlugin, HomeAssistantEventsPlugin))
            .add_systems(PostStartup, ha_post_startup_discover)
            .add_systems(Update, pump_ha_discover_results);
    }
}
