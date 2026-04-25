//! Pure async HTTP client for the IronClaw gateway (`:3000`). No Bevy types —
//! this module only knows about `reqwest`, `serde_json`, and the DTOs in
//! [`super::types`]. The Bevy-facing plumbing lives in
//! `crate::plugins::ironclaw_chat`.
//!
//! Auth: every call carries `Authorization: Bearer <token>`. The gateway also
//! accepts `?token=` on SSE for browsers that can't set headers; we always use
//! the header path.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use reqwest::{Client, StatusCode};
use reqwest_eventsource::EventSource;

use super::types::{
    HistoryResponse, ImageData, SendMessageRequest, SendMessageResponse, ThreadInfo,
    ThreadListResponse,
};

#[derive(Debug)]
pub enum GatewayError {
    Http(reqwest::Error),
    Status { code: StatusCode, body: String },
    Io(std::io::Error),
    MissingMime(String),
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GatewayError::Http(e) => write!(f, "http: {e}"),
            GatewayError::Status { code, body } => {
                let trimmed = body.chars().take(240).collect::<String>();
                write!(f, "gateway {code}: {trimmed}")
            }
            GatewayError::Io(e) => write!(f, "io: {e}"),
            GatewayError::MissingMime(p) => write!(f, "cannot infer mime from path: {p}"),
        }
    }
}

impl std::error::Error for GatewayError {}

impl From<reqwest::Error> for GatewayError {
    fn from(e: reqwest::Error) -> Self {
        GatewayError::Http(e)
    }
}

impl From<std::io::Error> for GatewayError {
    fn from(e: std::io::Error) -> Self {
        GatewayError::Io(e)
    }
}

/// Returns `Err` if the bearer token was rejected (401/403). Callers use this
/// to stop the SSE reconnect loop to avoid log spam.
pub fn is_auth_failure(err: &GatewayError) -> bool {
    matches!(
        err,
        GatewayError::Status {
            code: StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN,
            ..
        }
    )
}

#[derive(Clone)]
pub struct GatewayClient {
    base_url: String,
    bearer: String,
    http: Client,
}

impl GatewayClient {
    pub fn new(base_url: impl Into<String>, bearer: impl Into<String>, timeout_ms: u64) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_millis(timeout_ms.max(1_000)))
            .build()
            .expect("build reqwest client");
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            bearer: bearer.into(),
            http,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn has_bearer(&self) -> bool {
        !self.bearer.is_empty()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.bearer.is_empty() {
            req
        } else {
            req.bearer_auth(&self.bearer)
        }
    }

    // -- Threads ---------------------------------------------------------------

    pub async fn list_threads(&self) -> Result<ThreadListResponse, GatewayError> {
        let resp = self
            .auth(self.http.get(self.url("/api/chat/threads")))
            .send()
            .await?;
        parse_json::<ThreadListResponse>(resp).await
    }

    pub async fn create_thread(&self) -> Result<ThreadInfo, GatewayError> {
        let resp = self
            .auth(self.http.post(self.url("/api/chat/thread/new")))
            .send()
            .await?;
        parse_json::<ThreadInfo>(resp).await
    }

    pub async fn history(
        &self,
        thread_id: &str,
        limit: Option<u32>,
        before: Option<&str>,
    ) -> Result<HistoryResponse, GatewayError> {
        let mut req = self
            .http
            .get(self.url("/api/chat/history"))
            .query(&[("thread_id", thread_id)]);
        if let Some(limit) = limit {
            req = req.query(&[("limit", limit.to_string())]);
        }
        if let Some(before) = before {
            req = req.query(&[("before", before)]);
        }
        let resp = self.auth(req).send().await?;
        parse_json::<HistoryResponse>(resp).await
    }

    // -- Chat ------------------------------------------------------------------

    pub async fn send_message(
        &self,
        body: &SendMessageRequest,
    ) -> Result<SendMessageResponse, GatewayError> {
        let resp = self
            .auth(self.http.post(self.url("/api/chat/send")).json(body))
            .send()
            .await?;
        parse_json::<SendMessageResponse>(resp).await
    }

    /// Read a file from disk, base64-encode it, and infer a MIME from the
    /// extension. Used by the debug-UI "attach image" field.
    pub fn attach_file(path: impl AsRef<Path>) -> Result<ImageData, GatewayError> {
        let path = path.as_ref();
        let mime = guess_mime(path)
            .ok_or_else(|| GatewayError::MissingMime(path.display().to_string()))?;
        let bytes = std::fs::read(path)?;
        Ok(ImageData {
            media_type: mime.into(),
            data: B64.encode(bytes),
        })
    }

    // -- SSE -------------------------------------------------------------------

    /// Build an `EventSource` bound to `/api/chat/events`. The returned stream
    /// handles retry + backoff internally; callers drive it with `.next().await`
    /// and route the decoded `Event::Message` payloads through
    /// [`super::types::parse_app_event`].
    ///
    /// `last_event_id` — pass the id of the last event we processed to resume
    /// the stream after a disconnect (gateway honours both `Last-Event-ID`
    /// header and `?last_event_id=` query param).
    pub fn open_event_stream(&self, last_event_id: Option<&str>) -> EventSource {
        let mut req = self.http.get(self.url("/api/chat/events"));
        if !self.bearer.is_empty() {
            req = req.bearer_auth(&self.bearer);
        }
        if let Some(id) = last_event_id {
            req = req
                .header("Last-Event-ID", id)
                .query(&[("last_event_id", id)]);
        }
        EventSource::new(req).expect("event-source builder: request is always cloneable here")
    }
}

async fn parse_json<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, GatewayError> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp.json::<T>().await?)
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(GatewayError::Status { code: status, body })
    }
}

fn guess_mime(path: &Path) -> Option<&'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => return None,
    })
}
