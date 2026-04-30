//! Bevy plugin that spins up the RMCP streamable-HTTP server on a background
//! Tokio runtime. The plugin boots in `PostStartup` so by the time the HTTP
//! service is reachable, the [`PoseDriverPlugin`] and [`ChannelHubPlugin`]
//! have already inserted their shared resources (command queue, bone
//! snapshot, hub broadcast).
//!
//! The bridge is intentionally one-way on the hot path: tool handlers never
//! block the Bevy main thread — they enqueue a [`PoseCommand`] or fire a
//! `HubBroadcast::send`, and the Bevy side picks it up on the next
//! `PostUpdate` tick.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use bevy::prelude::*;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::net::TcpListener;
use tokio::runtime::Builder;

use jarvis_avatar::config::Settings;
use jarvis_avatar::paths::expand_home;
use jarvis_avatar::pose_library::PoseLibrary;

use super::{JarvisMcpServer, KimodoDefaults, build_a2f_client};
use crate::plugins::channel_server::HubBroadcast;
use crate::plugins::pose_capture::CaptureCommandSender;
use crate::plugins::pose_driver::{BoneSnapshotHandle, PoseCommandSender, PoseDriverPlugin};
use crate::plugins::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};

/// Plugin that boots the MCP server alongside the rest of the app.
pub struct McpPlugin;

impl Plugin for McpPlugin {
    fn build(&self, app: &mut App) {
        // Ensure the pose driver is registered (idempotent — Bevy silently skips duplicates).
        if !app.is_plugin_added::<PoseDriverPlugin>() {
            app.add_plugins(PoseDriverPlugin);
        }
        app.add_systems(PostStartup, start_mcp_server);
    }
}

fn start_mcp_server(
    settings: Res<Settings>,
    hub: Option<Res<HubBroadcast>>,
    pose_tx: Option<Res<PoseCommandSender>>,
    capture_tx: Option<Res<CaptureCommandSender>>,
    snapshot: Option<Res<BoneSnapshotHandle>>,
    traffic: Option<Res<TrafficLogSink>>,
) {
    if !settings.mcp.enabled {
        info!("mcp: disabled in config, not starting");
        return;
    }

    let Some(hub) = hub else {
        error!("mcp: HubBroadcast resource missing — ChannelHubPlugin must be loaded first");
        return;
    };
    let Some(pose_tx) = pose_tx else {
        error!("mcp: PoseCommandSender missing — PoseDriverPlugin must be loaded first");
        return;
    };
    let Some(snapshot) = snapshot else {
        error!("mcp: BoneSnapshotHandle missing — PoseDriverPlugin must be loaded first");
        return;
    };
    let Some(capture_tx) = capture_tx else {
        error!("mcp: CaptureCommandSender missing — PoseCapturePlugin must be loaded first");
        return;
    };

    let bind = settings.mcp.bind_address.clone();
    let path = settings.mcp.path.clone();
    let auth_token = settings.mcp.auth_token.clone();
    let session_keep_alive_sec = settings.mcp.session_keep_alive_sec;

    let poses_dir = expand_home(&settings.pose_library.poses_dir);
    let animations_dir = expand_home(&settings.pose_library.animations_dir);
    let library = PoseLibrary::new(poses_dir, animations_dir);

    let pose_guide_path = PathBuf::from("assets/POSE_GUIDE.md");
    let a2f = build_a2f_client(
        settings.a2f.enabled,
        settings.a2f.endpoint.clone(),
        settings.a2f.health_url.clone(),
        settings.a2f.function_id.clone(),
    );
    let kimodo_defaults = KimodoDefaults {
        duration_sec: settings.kimodo.default_duration_sec,
        steps: settings.kimodo.default_steps,
        timeout_sec: settings.kimodo.generate_timeout_sec,
    };
    let tts_kokoro_url = settings.tts.kokoro_url.clone();
    let tts_voice = settings.tts.voice.clone();
    let tts_enabled = settings.tts.enabled;
    let tts_response_format = settings.tts.response_format.clone();
    let tts_stream = settings.tts.stream;
    let tts_pcm_sample_rate = settings.tts.pcm_sample_rate;

    let hub_val = hub.clone();
    let pose_tx_val = pose_tx.clone();
    let capture_tx_val = capture_tx.clone();
    let snapshot_val = snapshot.clone();
    let traffic = traffic.map(|t| (*t).clone());

    thread::Builder::new()
        .name("jarvis-mcp".into())
        .spawn(move || {
            let rt = match Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("mcp: tokio runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(run_mcp_server(
                bind,
                path,
                auth_token,
                session_keep_alive_sec,
                JarvisMcpServer::new(
                    pose_tx_val,
                    capture_tx_val,
                    snapshot_val,
                    hub_val,
                    a2f,
                    pose_guide_path,
                    library,
                    kimodo_defaults,
                    tts_kokoro_url.clone(),
                    tts_voice.clone(),
                    tts_enabled,
                    tts_response_format.clone(),
                    tts_stream,
                    tts_pcm_sample_rate,
                    traffic.clone(),
                ),
                traffic,
            ));
        })
        .expect("failed to spawn jarvis-mcp thread");
}

async fn mcp_http_trace(
    State(traffic): State<Option<TrafficLogSink>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let resp = next.run(req).await;
    if let Some(ref log) = traffic {
        log.push(
            TrafficChannel::McpHttp,
            TrafficDirection::Inbound,
            format!("{method} {path} -> {}", resp.status()),
            None,
        );
    }
    resp
}

async fn run_mcp_server(
    bind: String,
    path: String,
    auth_token: String,
    session_keep_alive_sec: u64,
    server: JarvisMcpServer,
    traffic: Option<TrafficLogSink>,
) {
    let server_factory = move || Ok::<_, std::io::Error>(server.clone());

    let session_keep_alive =
        (session_keep_alive_sec > 0).then(|| Duration::from_secs(session_keep_alive_sec));
    let mut session_manager = LocalSessionManager::default();
    session_manager.session_config.keep_alive = session_keep_alive;
    let session_manager = std::sync::Arc::new(session_manager);

    let streamable: StreamableHttpService<JarvisMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            server_factory,
            session_manager,
            StreamableHttpServerConfig::default()
                .with_sse_keep_alive(Some(Duration::from_secs(15))),
        );

    let path_prefix = if path.starts_with('/') {
        path.clone()
    } else {
        format!("/{path}")
    };

    let mut app = Router::new().nest_service(&path_prefix, streamable);
    if !auth_token.is_empty() {
        let token = auth_token.clone();
        app = app.layer(middleware::from_fn_with_state(
            BearerAuth(token),
            require_bearer,
        ));
    }
    app = app.layer(middleware::from_fn_with_state(traffic, mcp_http_trace));

    let addr: SocketAddr = match bind.parse() {
        Ok(a) => a,
        Err(e) => {
            error!("mcp: invalid bind_address '{bind}': {e}");
            return;
        }
    };
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("mcp: bind {bind} failed: {e}");
            return;
        }
    };
    info!(
        "mcp: streamable-http listening on {bind}{path_prefix} (auth: {}; session_keep_alive_sec: {session_keep_alive_sec})",
        if auth_token.is_empty() {
            "none"
        } else {
            "bearer"
        },
    );
    if let Err(e) = axum::serve(listener, app).await {
        error!("mcp: axum serve exited: {e}");
    }
}

#[derive(Clone)]
struct BearerAuth(String);

async fn require_bearer(
    State(BearerAuth(expected)): State<BearerAuth>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ok = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            let prefix = "Bearer ";
            s.starts_with(prefix) && &s[prefix.len()..] == expected
        })
        .unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        let mut resp = Response::new(Body::from("unauthorized"));
        *resp.status_mut() = StatusCode::UNAUTHORIZED;
        resp.headers_mut().insert(
            "WWW-Authenticate",
            HeaderValue::from_static("Bearer realm=\"jarvis-mcp\""),
        );
        resp
    }
}
