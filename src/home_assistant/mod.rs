//! Home Assistant REST discovery (parity with Airi `ha-device-registry` store).
//!
//! Supports direct `Authorization: Bearer` calls to `{ha_url}/api/...` or the
//! ha-voice-bridge proxy: `{bridge_url}/ha-proxy/...` with `X-HA-URL` + `X-HA-Token`.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::config::HomeAssistantSettings;

#[derive(Debug, thiserror::Error)]
pub enum HaError {
    #[error("configure ha_url and ha_token (or bridge_url + ha_url + ha_token)")]
    NotConfigured,
    #[error("HTTP {0}: {1}")]
    Http(u16, String),
    #[error("request: {0}")]
    Request(#[from] reqwest::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct HaDeviceEntity {
    pub entity_id: String,
    pub label: String,
    pub area: String,
    pub domain: String,
    pub state: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HaCameraEntity {
    pub entity_id: String,
    pub label: String,
    pub area: String,
    pub domain: String,
    pub state: Option<String>,
    /// Relative HA path for snapshots (direct or via `/ha-camera` on bridge).
    pub snapshot_path: String,
}

#[derive(Debug, Clone)]
pub struct HaMediaEntity {
    pub entity_id: String,
    pub label: String,
    pub area: String,
    pub domain: String,
    pub state: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HaDetectionSensorEntity {
    pub entity_id: String,
    pub label: String,
    pub area: String,
    pub domain: String,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DiscoverySnapshot {
    pub cameras: Vec<HaCameraEntity>,
    pub mics: Vec<HaMediaEntity>,
    pub speakers: Vec<HaMediaEntity>,
    pub detection_sensors: Vec<HaDetectionSensorEntity>,
    pub all_areas: Vec<String>,
}

#[derive(Deserialize)]
struct HaStateRow {
    entity_id: String,
    state: String,
    #[serde(default)]
    attributes: Value,
}

#[derive(Debug, Clone)]
struct EntityRegRow {
    entity_id: String,
    area_id: Option<String>,
    device_id: Option<String>,
}

#[derive(Debug, Clone)]
struct DeviceRegRow {
    id: String,
    area_id: Option<String>,
}

/// REST `config/*/list` responses are usually a JSON array; some proxies / HA builds wrap as
/// `{ "result": [ ... ] }`. Without unwrapping, registry parses fail and every **Area** column is blank.
fn unwrap_ha_config_list(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        Value::Object(map) => map
            .get("result")
            .and_then(|x| x.as_array())
            .cloned()
            .or_else(|| {
                map.get("data")
                    .and_then(|x| x.as_array())
                    .cloned()
            })
            .unwrap_or_default(),
        _ => vec![],
    }
}

fn parse_area_name_map(items: &[Value]) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    for item in items {
        let id = item
            .get("area_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str);
        let name = item.get("name").and_then(Value::as_str);
        if let (Some(id), Some(name)) = (id, name) {
            m.insert(id.to_string(), name.to_string());
        }
    }
    m
}

fn parse_entity_registry_rows(items: &[Value]) -> Vec<EntityRegRow> {
    items
        .iter()
        .filter_map(|v| {
            let entity_id = v.get("entity_id")?.as_str()?.to_string();
            let area_id = v.get("area_id").and_then(Value::as_str).map(str::to_owned);
            let device_id = v.get("device_id").and_then(Value::as_str).map(str::to_owned);
            Some(EntityRegRow {
                entity_id,
                area_id,
                device_id,
            })
        })
        .collect()
}

fn parse_device_registry_rows(items: &[Value]) -> Vec<DeviceRegRow> {
    items
        .iter()
        .filter_map(|v| {
            let id = v.get("id")?.as_str()?.to_string();
            let area_id = v.get("area_id").and_then(Value::as_str).map(str::to_owned);
            Some(DeviceRegRow { id, area_id })
        })
        .collect()
}

fn trim_slash(s: &str) -> &str {
    s.trim_end_matches('/')
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Build a GET request for HA JSON APIs.
pub fn ha_get(
    client: &Client,
    settings: &HomeAssistantSettings,
    path: &str,
) -> reqwest::RequestBuilder {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let bridge = settings.bridge_url.trim();
    if !bridge.is_empty() {
        let url = format!("{}/ha-proxy{path}", trim_slash(bridge));
        client.get(url).header("X-HA-URL", settings.ha_url.trim()).header(
            "X-HA-Token",
            settings.ha_token.trim(),
        )
    } else {
        let base = trim_slash(settings.ha_url.trim());
        let url = format!("{base}{path}");
        client
            .get(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", settings.ha_token.trim()),
            )
            .header(reqwest::header::ACCEPT, "application/json")
    }
}

/// POST with JSON body (registry list endpoints sometimes require POST `{}`).
pub fn ha_post_json(
    client: &Client,
    settings: &HomeAssistantSettings,
    path: &str,
    body: &Value,
) -> reqwest::RequestBuilder {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let bridge = settings.bridge_url.trim();
    if !bridge.is_empty() {
        let url = format!("{}/ha-proxy{path}", trim_slash(bridge));
        client
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-HA-URL", settings.ha_url.trim())
            .header("X-HA-Token", settings.ha_token.trim())
            .json(body)
    } else {
        let base = trim_slash(settings.ha_url.trim());
        let url = format!("{base}{path}");
        client
            .post(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", settings.ha_token.trim()),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
    }
}

/// Latest state object for a single entity (`GET /api/states/<entity_id>`).
pub async fn fetch_state(client: &Client, settings: &HomeAssistantSettings, entity_id: &str) -> Result<Value, HaError> {
    let path = format!("/api/states/{entity_id}");
    fetch_json_get(client, settings, &path).await
}

async fn fetch_json_get(client: &Client, settings: &HomeAssistantSettings, path: &str) -> Result<Value, HaError> {
    let resp = ha_get(client, settings, path).send().await?;
    let status = resp.status();
    let txt = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(HaError::Http(
            status.as_u16(),
            txt.chars().take(500).collect(),
        ));
    }
    Ok(serde_json::from_str(&txt)?)
}

async fn fetch_json_post_or_get(
    client: &Client,
    settings: &HomeAssistantSettings,
    path: &str,
) -> Result<Value, HaError> {
    match fetch_json_get(client, settings, path).await {
        Ok(v) => Ok(v),
        Err(HaError::Http(code, _)) if code == 405 || code == 400 => {
            let resp = ha_post_json(client, settings, path, &Value::Object(Default::default()))
                .send()
                .await?;
            let status = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(HaError::Http(
                    status.as_u16(),
                    txt.chars().take(500).collect(),
                ));
            }
            Ok(serde_json::from_str(&txt)?)
        }
        Err(e) => Err(e),
    }
}

fn friendly_name(entity_id: &str, attributes: &Value) -> String {
    attributes
        .get("friendly_name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| entity_id.to_string())
}

/// Full device scan (cameras, mics, speakers, detection sensors + areas).
pub async fn discover(client: &Client, settings: &HomeAssistantSettings) -> Result<DiscoverySnapshot, HaError> {
    if settings.ha_url.trim().is_empty() || settings.ha_token.trim().is_empty() {
        return Err(HaError::NotConfigured);
    }

    let states_v = fetch_json_get(client, settings, "/api/states").await?;
    let states: Vec<HaStateRow> = serde_json::from_value(states_v)?;

    let area_raw = fetch_json_post_or_get(client, settings, "/api/config/area_registry/list")
        .await
        .unwrap_or_else(|_| Value::Array(vec![]));
    let area_items = unwrap_ha_config_list(area_raw);
    let area_map = parse_area_name_map(&area_items);

    let entity_raw = fetch_json_post_or_get(client, settings, "/api/config/entity_registry/list")
        .await
        .unwrap_or_else(|_| Value::Array(vec![]));
    let entity_registry = parse_entity_registry_rows(&unwrap_ha_config_list(entity_raw));

    let device_raw = fetch_json_post_or_get(client, settings, "/api/config/device_registry/list")
        .await
        .unwrap_or_else(|_| Value::Array(vec![]));
    let device_registry = parse_device_registry_rows(&unwrap_ha_config_list(device_raw));

    fn resolve_area(
        entity_id: &str,
        entity_registry: &[EntityRegRow],
        device_registry: &[DeviceRegRow],
        area_map: &std::collections::HashMap<String, String>,
    ) -> String {
        if let Some(e) = entity_registry.iter().find(|e| e.entity_id == entity_id) {
            if let Some(ref aid) = e.area_id {
                return area_map.get(aid).cloned().unwrap_or_else(|| aid.clone());
            }
            if let Some(ref did) = e.device_id {
                if let Some(d) = device_registry.iter().find(|d| d.id == *did) {
                    if let Some(ref aid) = d.area_id {
                        return area_map.get(aid).cloned().unwrap_or_else(|| aid.clone());
                    }
                }
            }
        }
        String::new()
    }

    let mut cameras = Vec::new();
    let mut mics = Vec::new();
    let mut speakers = Vec::new();
    let mut detections = Vec::new();

    for entity in &states {
        let domain = entity.entity_id.split('.').next().unwrap_or("");
        let area = resolve_area(
            &entity.entity_id,
            &entity_registry,
            &device_registry,
            &area_map,
        );
        let label = friendly_name(&entity.entity_id, &entity.attributes);

        if domain == "camera" {
            cameras.push(HaCameraEntity {
                entity_id: entity.entity_id.clone(),
                label,
                area,
                domain: "camera".into(),
                state: Some(entity.state.clone()),
                snapshot_path: format!("/api/camera_proxy/{}", entity.entity_id),
            });
            continue;
        }

        if domain == "assist_satellite" {
            mics.push(HaMediaEntity {
                entity_id: entity.entity_id.clone(),
                label: friendly_name(&entity.entity_id, &entity.attributes),
                area: resolve_area(
                    &entity.entity_id,
                    &entity_registry,
                    &device_registry,
                    &area_map,
                ),
                domain: "assist_satellite".into(),
                state: Some(entity.state.clone()),
            });
        }

        if domain == "media_player" || domain == "assist_satellite" {
            speakers.push(HaMediaEntity {
                entity_id: entity.entity_id.clone(),
                label: friendly_name(&entity.entity_id, &entity.attributes),
                area: resolve_area(
                    &entity.entity_id,
                    &entity_registry,
                    &device_registry,
                    &area_map,
                ),
                domain: domain.to_string(),
                state: Some(entity.state.clone()),
            });
        }

        if domain == "sensor" && entity.entity_id.contains("detect") {
            detections.push(HaDetectionSensorEntity {
                entity_id: entity.entity_id.clone(),
                label: friendly_name(&entity.entity_id, &entity.attributes),
                area: resolve_area(
                    &entity.entity_id,
                    &entity_registry,
                    &device_registry,
                    &area_map,
                ),
                domain: "sensor".into(),
                state: Some(entity.state.clone()),
            });
        }
    }

    let mut area_names: HashSet<String> = area_map.values().cloned().collect();
    for c in &cameras {
        if !c.area.is_empty() {
            area_names.insert(c.area.clone());
        }
    }
    for m in &mics {
        if !m.area.is_empty() {
            area_names.insert(m.area.clone());
        }
    }
    for s in &speakers {
        if !s.area.is_empty() {
            area_names.insert(s.area.clone());
        }
    }
    let mut all_areas: Vec<String> = area_names.into_iter().filter(|s| !s.is_empty()).collect();
    all_areas.sort();

    Ok(DiscoverySnapshot {
        cameras,
        mics,
        speakers,
        detection_sensors: detections,
        all_areas,
    })
}

/// Toggle helpers — mutate the enabled id list in settings.
pub fn toggle_id(list: &mut Vec<String>, id: &str) {
    if let Some(i) = list.iter().position(|x| x == id) {
        list.remove(i);
    } else {
        list.push(id.to_string());
    }
}

pub fn configured(settings: &HomeAssistantSettings) -> bool {
    !settings.ha_url.trim().is_empty() && !settings.ha_token.trim().is_empty()
}

pub fn discovery_timestamp_ms() -> u128 {
    now_ms()
}
