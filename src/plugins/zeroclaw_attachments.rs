//! Tiny HTTP server that serves ephemeral attachments to ZeroClaw.
//!
//! ZeroClaw's chat surfaces (`/webhook`, `/ws/chat`) take no binary payload.
//! To get images into a ZeroClaw multimodal agent we host them ourselves and
//! inline a URL into the outbound message text — the agent's built-in HTTP
//! tool then fetches the bytes.
//!
//! Flow:
//! 1. The chat plugin calls [`publish_images`] with one or more
//!    [`ImageData`] payloads.
//! 2. Each image is decoded from base64, written under
//!    `<temp>/jarvis-zeroclaw-attachments/<uuid>.<ext>`, and the public URL
//!    is returned.
//! 3. The HTTP server (bound to `[zeroclaw].attachments_bind`) serves the
//!    files at `/attachments/<uuid>.<ext>`. Older files are evicted when the
//!    on-disk count crosses `attachments_max`.
//!
//! The server is started unconditionally so the chat plugin can call
//! [`publish_images`] without first checking liveness; if the gateway never
//! actually uses ZeroClaw the port is just an idle axum router.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use bevy::prelude::*;
use once_cell::sync::OnceCell;
use tokio::net::TcpListener;
use tokio::runtime::Builder;

use crate::config::{ChatBackend, Settings, ZeroClawSettings};
use crate::ironclaw::types::ImageData;

/// Process-global handle to the attachments registry so the chat plugin can
/// reach it from inside a tokio worker without funneling through Bevy.
static ATTACHMENTS: OnceCell<AttachmentRegistry> = OnceCell::new();

#[derive(Clone)]
struct AttachmentRegistry {
    storage_dir: PathBuf,
    public_base: String,
    state: Arc<Mutex<RegistryState>>,
}

struct RegistryState {
    max_files: usize,
    order: VecDeque<PathBuf>,
}

impl AttachmentRegistry {
    fn new(storage_dir: PathBuf, public_base: String, max_files: usize) -> Self {
        Self {
            storage_dir,
            public_base,
            state: Arc::new(Mutex::new(RegistryState {
                max_files: max_files.max(1),
                order: VecDeque::new(),
            })),
        }
    }

    fn publish(&self, ext: &str, bytes: &[u8]) -> Result<(PathBuf, String), String> {
        let id = uuid::Uuid::new_v4();
        let filename = format!("{id}.{ext}");
        let path = self.storage_dir.join(&filename);
        std::fs::write(&path, bytes).map_err(|e| format!("write attachment: {e}"))?;
        let mut state = self.state.lock().map_err(|_| "poisoned mutex")?;
        state.order.push_back(path.clone());
        while state.order.len() > state.max_files {
            if let Some(stale) = state.order.pop_front() {
                let _ = std::fs::remove_file(stale);
            }
        }
        Ok((path, format!("{}/attachments/{}", self.public_base, filename)))
    }
}

pub struct ZeroClawAttachmentsPlugin;

impl Plugin for ZeroClawAttachmentsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_attachments_server);
    }
}

fn spawn_attachments_server(settings: Res<Settings>) {
    // The server only matters when ZeroClaw is the active chat backend AND
    // attachments are enabled. Either condition fails → no server, no
    // wasted port. The registry stays uninitialized so `publish_images` is a
    // no-op fallthrough.
    if !matches!(
        ChatBackend::parse(&settings.gateway.backend),
        ChatBackend::Zeroclaw,
    ) {
        return;
    }
    let cfg = settings.zeroclaw.clone();
    if !cfg.attachments_enabled {
        return;
    }

    let storage_dir = std::env::temp_dir().join("jarvis-zeroclaw-attachments");
    if let Err(e) = std::fs::create_dir_all(&storage_dir) {
        warn!(
            "zeroclaw_attachments: failed to create {}: {e}",
            storage_dir.display()
        );
        return;
    }
    // Best-effort sweep of any leftovers from a previous run so we don't
    // grow without bound across restarts.
    sweep_existing(&storage_dir);

    let public_base = resolve_public_base(&cfg);
    let registry = AttachmentRegistry::new(
        storage_dir.clone(),
        public_base.clone(),
        cfg.attachments_max as usize,
    );
    if ATTACHMENTS.set(registry.clone()).is_err() {
        warn!("zeroclaw_attachments: registry already initialised; skipping");
        return;
    }

    let bind = cfg.attachments_bind.clone();
    info!(
        "zeroclaw_attachments: serving from {} at http://{} (public {})",
        storage_dir.display(),
        bind,
        public_base
    );
    std::thread::Builder::new()
        .name("jarvis-zc-attachments".into())
        .spawn(move || {
            let rt = match Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("zeroclaw_attachments runtime build: {e}");
                    return;
                }
            };
            rt.block_on(run_server(bind, registry));
        })
        .ok();
}

async fn run_server(bind: String, registry: AttachmentRegistry) {
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/attachments/{file}", get(serve_attachment))
        .with_state(registry);
    let listener = match TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            error!("zeroclaw_attachments: bind {bind}: {e}");
            return;
        }
    };
    // Graceful shutdown on SIGINT/SIGTERM so the std::thread holding this
    // tokio runtime returns cleanly instead of being torn down by process
    // exit. Without this the bevy/winit close path could deadlock against
    // the runtime drop if any in-flight request was still being handled.
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        error!("zeroclaw_attachments: serve: {e}");
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn serve_attachment(
    State(reg): State<AttachmentRegistry>,
    AxumPath(file): AxumPath<String>,
) -> impl IntoResponse {
    // Sanitize the filename: no `..`, no path separators. We control the
    // filename generation (uuid + ext) but defensive checks are cheap.
    if file.contains('/') || file.contains('\\') || file.contains("..") {
        return (StatusCode::BAD_REQUEST, "invalid path".to_string()).into_response();
    }
    let path = reg.storage_dir.join(&file);
    if !path.starts_with(&reg.storage_dir) {
        return (StatusCode::BAD_REQUEST, "invalid path".to_string()).into_response();
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => {
            return (StatusCode::NOT_FOUND, "not found".to_string()).into_response();
        }
    };
    let mime = mime_for_path(&path);
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(mime) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    // Cache for a short window — these URLs are intended for single-fetch
    // agent use, not browser caches.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=60"),
    );
    (StatusCode::OK, headers, bytes).into_response()
}

fn mime_for_path(p: &Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
}

fn sweep_existing(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

fn resolve_public_base(cfg: &ZeroClawSettings) -> String {
    let override_url = cfg.attachments_public_url.trim().trim_end_matches('/');
    if !override_url.is_empty() {
        return override_url.to_string();
    }
    // Derive from `attachments_bind` — replace 0.0.0.0 / :: with a real LAN
    // IP so the URL is reachable from the ZeroClaw host.
    let (host, port) = parse_bind(&cfg.attachments_bind);
    let host = if host == "0.0.0.0" || host == "::" || host.is_empty() {
        detect_lan_ip().unwrap_or_else(|| "127.0.0.1".to_string())
    } else {
        host
    };
    format!("http://{host}:{port}")
}

fn parse_bind(s: &str) -> (String, u16) {
    if let Some((h, p)) = s.rsplit_once(':') {
        let port = p.parse::<u16>().unwrap_or(6124);
        let host = h.trim_start_matches('[').trim_end_matches(']');
        return (host.to_string(), port);
    }
    (String::new(), 6124)
}

fn detect_lan_ip() -> Option<String> {
    use std::net::UdpSocket;
    // Trick: open a UDP socket pointed at a public-ish destination — the
    // kernel picks the local interface address without sending anything.
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("1.1.1.1:80").ok()?;
    let addr = sock.local_addr().ok()?;
    Some(addr.ip().to_string())
}

// ---------- Public API for the chat plugin -----------------------------------

/// Decode the supplied [`ImageData`] payloads, persist them to the
/// attachments directory, and return their public URLs. Returns `None` when
/// the registry is not initialised (attachments disabled, or ZeroClaw not the
/// active backend) so callers can transparently skip the inlining step.
pub fn publish_images(images: &[ImageData]) -> Option<Vec<String>> {
    let reg = ATTACHMENTS.get()?;
    let mut urls = Vec::with_capacity(images.len());
    for image in images {
        let ext = mime_to_ext(&image.media_type);
        let Ok(bytes) = B64.decode(image.data.as_bytes()) else {
            warn!("zeroclaw_attachments: skipping image with invalid base64");
            continue;
        };
        match reg.publish(ext, &bytes) {
            Ok((_path, url)) => urls.push(url),
            Err(e) => warn!("zeroclaw_attachments: publish failed: {e}"),
        }
    }
    if urls.is_empty() { None } else { Some(urls) }
}

fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bind_splits_host_port() {
        assert_eq!(parse_bind("0.0.0.0:6124"), ("0.0.0.0".into(), 6124));
        assert_eq!(parse_bind("192.168.1.5:80"), ("192.168.1.5".into(), 80));
        assert_eq!(parse_bind(":6124"), ("".into(), 6124));
    }

    #[test]
    fn mime_to_ext_basic() {
        assert_eq!(mime_to_ext("image/png"), "png");
        assert_eq!(mime_to_ext("image/jpeg"), "jpg");
        assert_eq!(mime_to_ext("image/unknown"), "bin");
    }

    #[test]
    fn resolve_public_base_honors_override() {
        let cfg = ZeroClawSettings {
            attachments_public_url: "https://avatar.example/  ".into(),
            ..Default::default()
        };
        assert_eq!(resolve_public_base(&cfg), "https://avatar.example");
    }
}
