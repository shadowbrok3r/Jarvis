//! Presence-style routing: pick active camera / mic / speaker by area (Airi parity).
//!
//! Vision-derived area and HA room slug consumers can be wired later; until then
//! `vision_active_area` stays empty unless tests set it, and `forced_area` overrides.

use bevy::prelude::*;

use jarvis_avatar::config::HomeAssistantSettings;
use jarvis_avatar::home_assistant::{
    DiscoverySnapshot, HaCameraEntity, HaDetectionSensorEntity, HaMediaEntity,
};

/// How long HA-reported room slug wins over vision (`presence-router.ts`).
pub const HA_ROOM_AUTHORITY_TTL_MS: u128 = 120_000;

#[derive(Resource, Default)]
pub struct PresenceRouting {
    pub ha_room_slug: String,
    pub ha_room_updated_at_ms: u128,
    /// Filled when a vision pipeline exists; empty = no detections.
    pub vision_active_area: String,
    /// Debug override: when non-empty, used as the logical area (clears HA authority semantics).
    pub forced_area: String,
    #[allow(dead_code)]
    pub last_route_change_at_ms: Option<u128>,
}

pub fn now_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Effective area string used for device selection.
pub fn resolve_active_area(routing: &PresenceRouting, now_ms: u128) -> String {
    let fa = routing.forced_area.trim();
    if !fa.is_empty() {
        return fa.to_string();
    }

    let slug = routing.ha_room_slug.trim();
    if !slug.is_empty()
        && now_ms.saturating_sub(routing.ha_room_updated_at_ms) <= HA_ROOM_AUTHORITY_TTL_MS
    {
        return slug.to_string();
    }

    routing.vision_active_area.trim().to_string()
}

pub struct DevicesForArea<'a> {
    pub cameras: Vec<&'a HaCameraEntity>,
    pub mics: Vec<&'a HaMediaEntity>,
    pub speakers: Vec<&'a HaMediaEntity>,
    #[allow(dead_code)]
    pub detection_sensors: Vec<&'a HaDetectionSensorEntity>,
}

pub fn get_devices_for_area<'a>(
    area: &str,
    snapshot: &'a DiscoverySnapshot,
    settings: &HomeAssistantSettings,
) -> DevicesForArea<'a> {
    let cameras = snapshot
        .cameras
        .iter()
        .filter(|c| settings.enabled_camera_ids.contains(&c.entity_id) && c.area == area)
        .collect();
    let mics = snapshot
        .mics
        .iter()
        .filter(|m| settings.enabled_mic_ids.contains(&m.entity_id) && m.area == area)
        .collect();
    let speakers = snapshot
        .speakers
        .iter()
        .filter(|s| settings.enabled_speaker_ids.contains(&s.entity_id) && s.area == area)
        .collect();
    let detection_sensors = snapshot
        .detection_sensors
        .iter()
        .filter(|s| settings.detection_sensor_ids.contains(&s.entity_id) && s.area == area)
        .collect();
    DevicesForArea {
        cameras,
        mics,
        speakers,
        detection_sensors,
    }
}

fn enabled_cameras<'a>(
    snapshot: &'a DiscoverySnapshot,
    settings: &HomeAssistantSettings,
) -> Vec<&'a HaCameraEntity> {
    snapshot
        .cameras
        .iter()
        .filter(|c| settings.enabled_camera_ids.contains(&c.entity_id))
        .collect()
}

fn enabled_mics<'a>(
    snapshot: &'a DiscoverySnapshot,
    settings: &HomeAssistantSettings,
) -> Vec<&'a HaMediaEntity> {
    snapshot
        .mics
        .iter()
        .filter(|m| settings.enabled_mic_ids.contains(&m.entity_id))
        .collect()
}

fn enabled_speakers<'a>(
    snapshot: &'a DiscoverySnapshot,
    settings: &HomeAssistantSettings,
) -> Vec<&'a HaMediaEntity> {
    snapshot
        .speakers
        .iter()
        .filter(|s| settings.enabled_speaker_ids.contains(&s.entity_id))
        .collect()
}

pub fn active_camera<'a>(
    snapshot: &'a DiscoverySnapshot,
    settings: &HomeAssistantSettings,
    routing: &PresenceRouting,
) -> Option<&'a HaCameraEntity> {
    let now = now_ms();
    let active = resolve_active_area(routing, now);
    let def = settings.default_area.trim();

    let area_key = if active.is_empty() {
        def
    } else {
        active.as_str()
    };
    if area_key.is_empty() {
        return enabled_cameras(snapshot, settings).into_iter().next();
    }

    let dev = get_devices_for_area(area_key, snapshot, settings);
    dev.cameras
        .into_iter()
        .next()
        .or_else(|| enabled_cameras(snapshot, settings).into_iter().next())
}

pub fn active_mic<'a>(
    snapshot: &'a DiscoverySnapshot,
    settings: &HomeAssistantSettings,
    routing: &PresenceRouting,
) -> Option<&'a HaMediaEntity> {
    let now = now_ms();
    let active = resolve_active_area(routing, now);
    let def = settings.default_area.trim();
    let area_key = if active.is_empty() {
        def
    } else {
        active.as_str()
    };
    if area_key.is_empty() {
        return enabled_mics(snapshot, settings).into_iter().next();
    }
    let dev = get_devices_for_area(area_key, snapshot, settings);
    dev.mics
        .into_iter()
        .next()
        .or_else(|| enabled_mics(snapshot, settings).into_iter().next())
}

pub fn active_speaker<'a>(
    snapshot: &'a DiscoverySnapshot,
    settings: &HomeAssistantSettings,
    routing: &PresenceRouting,
) -> Option<&'a HaMediaEntity> {
    let now = now_ms();
    let active = resolve_active_area(routing, now);
    let def = settings.default_area.trim();
    let area_key = if active.is_empty() {
        def
    } else {
        active.as_str()
    };
    if area_key.is_empty() {
        return enabled_speakers(snapshot, settings).into_iter().next();
    }
    let dev = get_devices_for_area(area_key, snapshot, settings);
    dev.speakers
        .into_iter()
        .next()
        .or_else(|| enabled_speakers(snapshot, settings).into_iter().next())
}

pub struct HomeAssistantRoutingPlugin;

impl Plugin for HomeAssistantRoutingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PresenceRouting>();
    }
}
