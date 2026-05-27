//! Pure async HTTP + WS + SSE client for the ZeroClaw gateway.
//!
//! Three surfaces:
//! 1. **HTTP** — `/webhook`, `/api/memory`, `/api/status`, `/health`. Built
//!    on `reqwest`. Every call attaches `Authorization: Bearer <token>` when
//!    a token is configured, plus `X-Client` + `User-Agent` for traceability.
//! 2. **WebSocket** — `/ws/chat`. The caller drives the
//!    [`tokio_tungstenite::WebSocketStream`] returned by
//!    [`ZeroClawClient::open_chat_ws`]; this module only handles the
//!    handshake.
//! 3. **SSE** — `/api/events`. Long-lived [`reqwest_eventsource::EventSource`]
//!    via [`ZeroClawClient::open_event_stream`].
//!
//! No Bevy types here — the Bevy-facing plumbing lives in
//! `crate::plugins::zeroclaw_chat` and `crate::plugins::zeroclaw_context`
//! (binary crate).

use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::{Client, StatusCode};
use reqwest_eventsource::EventSource;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Request as TungsteniteRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use super::types::{
    MemoryListResponse, MemoryWriteRequest, SessionListResponse, SessionMessagesResponse,
    StatusResponse, WebhookRequest, WebhookResponse,
};

const HEADER_CLIENT: &str = "X-Client";
const HEADER_WEBHOOK_SECRET: &str = "X-Webhook-Secret";
const HEADER_IDEMPOTENCY: &str = "X-Idempotency-Key";

#[derive(Debug)]
pub enum ZeroClawError {
    Http(reqwest::Error),
    Status { code: StatusCode, body: String },
    Url(String),
    Ws(tokio_tungstenite::tungstenite::Error),
}

impl fmt::Display for ZeroClawError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZeroClawError::Http(e) => write!(f, "http: {e}"),
            ZeroClawError::Status { code, body } => {
                let trimmed = body.chars().take(240).collect::<String>();
                write!(f, "zeroclaw {code}: {trimmed}")
            }
            ZeroClawError::Url(s) => write!(f, "url: {s}"),
            ZeroClawError::Ws(e) => write!(f, "ws: {e}"),
        }
    }
}

impl std::error::Error for ZeroClawError {}

impl From<reqwest::Error> for ZeroClawError {
    fn from(e: reqwest::Error) -> Self {
        ZeroClawError::Http(e)
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for ZeroClawError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        ZeroClawError::Ws(e)
    }
}

/// Returns `Err` if the bearer token was rejected (401/403). Callers stop the
/// SSE reconnect loop on this to avoid log spam.
pub fn is_auth_failure(err: &ZeroClawError) -> bool {
    matches!(
        err,
        ZeroClawError::Status {
            code: StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN,
            ..
        }
    )
}

/// Where attachments and out-of-band identification metadata get attached.
#[derive(Debug, Clone, Default)]
pub struct ClientIdentity {
    /// e.g. `"jarvis-avatar"`. Sent as `X-Client` + `User-Agent`. Empty =
    /// reqwest's default UA + no `X-Client` header.
    pub client_id: String,
    /// Optional `X-Webhook-Secret`, applied only to `/webhook` calls.
    pub webhook_secret: String,
}

#[derive(Clone)]
pub struct ZeroClawClient {
    base_url: String,
    ws_url: String,
    bearer: String,
    identity: ClientIdentity,
    agent_alias: String,
    http: Client,
}

impl ZeroClawClient {
    pub fn new(
        base_url: impl Into<String>,
        ws_url: impl Into<String>,
        bearer: impl Into<String>,
        identity: ClientIdentity,
        agent_alias: impl Into<String>,
        timeout_ms: u64,
    ) -> Self {
        let client_id = identity.client_id.clone();
        let user_agent = if client_id.is_empty() {
            "jarvis-avatar/0.1".to_string()
        } else {
            format!("{}/0.1", client_id)
        };
        let http = Client::builder()
            .timeout(Duration::from_millis(timeout_ms.max(1_000)))
            .user_agent(user_agent)
            .build()
            .expect("build reqwest client");
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            ws_url: ws_url.into().trim_end_matches('/').to_string(),
            bearer: bearer.into(),
            identity,
            agent_alias: agent_alias.into(),
            http,
        }
    }

    pub fn agent_alias(&self) -> &str {
        &self.agent_alias
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    pub fn has_bearer(&self) -> bool {
        !self.bearer.is_empty()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = req;
        if !self.bearer.is_empty() {
            req = req.bearer_auth(&self.bearer);
        }
        if !self.identity.client_id.is_empty() {
            if let Ok(v) = HeaderValue::from_str(&self.identity.client_id) {
                req = req.header(HEADER_CLIENT, v);
            }
        }
        req
    }

    // ---- /health (no auth required) -----------------------------------------

    pub async fn health(&self) -> Result<bool, ZeroClawError> {
        let resp = self.http.get(self.url("/health")).send().await?;
        Ok(resp.status().is_success())
    }

    // ---- /api/status --------------------------------------------------------

    pub async fn status(&self) -> Result<StatusResponse, ZeroClawError> {
        let resp = self.auth(self.http.get(self.url("/api/status"))).send().await?;
        parse_json::<StatusResponse>(resp).await
    }

    // ---- /webhook -----------------------------------------------------------

    /// Synchronous chat round-trip. Returns the full assistant reply (or the
    /// idempotency duplicate sentinel) inside [`WebhookResponse`].
    pub async fn webhook(
        &self,
        message: &str,
        idempotency_key: Option<&str>,
    ) -> Result<WebhookResponse, ZeroClawError> {
        let body = WebhookRequest {
            message: message.to_string(),
        };
        // Always pin the agent explicitly even though `/webhook` would
        // auto-pick the first enabled `[agents.<alias>]`. Matching the WS
        // path's requirement here keeps the two surfaces in sync, and
        // surfaces "no such agent" as a real 400 instead of a silent route
        // to the wrong agent.
        let mut url = self.http.post(self.url("/webhook"));
        if !self.agent_alias.is_empty() {
            url = url.query(&[("agent", &self.agent_alias)]);
        }
        let mut req = self.auth(url.json(&body));
        if !self.identity.webhook_secret.is_empty() {
            if let Ok(v) = HeaderValue::from_str(&self.identity.webhook_secret) {
                req = req.header(HEADER_WEBHOOK_SECRET, v);
            }
        }
        if let Some(key) = idempotency_key {
            if let Ok(v) = HeaderValue::from_str(key) {
                req = req.header(HEADER_IDEMPOTENCY, v);
            }
        }
        let resp = req.send().await?;
        parse_json::<WebhookResponse>(resp).await
    }

    // ---- /api/memory --------------------------------------------------------

    pub async fn memory_write(
        &self,
        req: &MemoryWriteRequest,
    ) -> Result<(), ZeroClawError> {
        let resp = self
            .auth(self.http.post(self.url("/api/memory")).json(req))
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(ZeroClawError::Status { code, body })
        }
    }

    pub async fn memory_list(
        &self,
        query: Option<&str>,
        category: Option<&str>,
    ) -> Result<MemoryListResponse, ZeroClawError> {
        let mut req = self.http.get(self.url("/api/memory"));
        if let Some(q) = query {
            req = req.query(&[("query", q)]);
        }
        if let Some(cat) = category {
            req = req.query(&[("category", cat)]);
        }
        let resp = self.auth(req).send().await?;
        parse_json::<MemoryListResponse>(resp).await
    }

    pub async fn memory_delete(&self, key: &str) -> Result<(), ZeroClawError> {
        let url = self.url(&format!("/api/memory/{}", urlencode_path(key)));
        let resp = self.auth(self.http.delete(url)).send().await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(ZeroClawError::Status { code, body })
        }
    }

    // ---- /api/sessions ------------------------------------------------------

    pub async fn list_sessions(&self) -> Result<SessionListResponse, ZeroClawError> {
        let resp = self
            .auth(self.http.get(self.url("/api/sessions")))
            .send()
            .await?;
        parse_json::<SessionListResponse>(resp).await
    }

    pub async fn session_messages(
        &self,
        session_id: &str,
    ) -> Result<SessionMessagesResponse, ZeroClawError> {
        // The handler accepts either `gw_<uuid>` or bare `<uuid>` — we pass
        // the bare form (what `list_sessions` returns in `session_id`).
        let url = self.url(&format!(
            "/api/sessions/{}/messages",
            urlencoding::encode(session_id)
        ));
        let resp = self.auth(self.http.get(url)).send().await?;
        parse_json::<SessionMessagesResponse>(resp).await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<(), ZeroClawError> {
        let url = self.url(&format!("/api/sessions/{}", urlencoding::encode(session_id)));
        let resp = self.auth(self.http.delete(url)).send().await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(ZeroClawError::Status { code, body })
        }
    }

    // ---- /api/events (SSE) --------------------------------------------------

    /// Build an `EventSource` bound to `/api/events`. The returned stream
    /// handles retry + backoff internally; the chat plugin drives it with
    /// `.next().await`.
    pub fn open_event_stream(&self) -> EventSource {
        let mut req = self.http.get(self.url("/api/events"));
        if !self.bearer.is_empty() {
            req = req.bearer_auth(&self.bearer);
        }
        if !self.identity.client_id.is_empty() {
            if let Ok(v) = HeaderValue::from_str(&self.identity.client_id) {
                req = req.header(HEADER_CLIENT, v);
            }
        }
        EventSource::new(req).expect("event-source builder: request is always cloneable here")
    }

    // ---- /ws/chat -----------------------------------------------------------

    /// Open a WebSocket to `/ws/chat?agent=<alias>&session_id=<id>&token=<bearer>`.
    ///
    /// `session_id` resumes the named gateway session (and creates it
    /// server-side on first use). Pass `None` to let ZeroClaw mint one —
    /// usually you want to manage it client-side so reopens stick.
    pub async fn open_chat_ws(
        &self,
        session_id: Option<&str>,
    ) -> Result<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, ZeroClawError> {
        // `/ws/chat` rejects the upgrade with HTTP 400 unless `?agent=` is
        // present and matches a configured `[agents.<alias>]` on the gateway.
        // Build the URL with `agent` always set, then layer optional
        // `session_id` + the token (browsers/WS stacks can't always set
        // Authorization on the upgrade so ZeroClaw also accepts ?token=).
        let mut url = format!(
            "{}/ws/chat?agent={}",
            self.ws_url,
            urlencoding::encode(&self.agent_alias)
        );
        if let Some(sid) = session_id.map(str::trim).filter(|s| !s.is_empty()) {
            url.push_str("&session_id=");
            url.push_str(&urlencoding::encode(sid));
        }
        if !self.bearer.is_empty() {
            url.push_str("&token=");
            url.push_str(&urlencoding::encode(&self.bearer));
        }
        let mut req: TungsteniteRequest<_> = url
            .as_str()
            .into_client_request()
            .map_err(|e| ZeroClawError::Url(e.to_string()))?;
        if !self.identity.client_id.is_empty() {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(HEADER_CLIENT.as_bytes()),
                HeaderValue::from_str(&self.identity.client_id),
            ) {
                req.headers_mut().insert(name, value);
            }
            if let Ok(v) = HeaderValue::from_str(&format!("{}/0.1", self.identity.client_id)) {
                req.headers_mut().insert(USER_AGENT, v);
            }
        }
        let (ws, _resp) = connect_async(req).await?;
        Ok(ws)
    }

    /// Helper used by the chat plugin to build a request-derived header map
    /// the traffic log can summarize without exposing the bearer.
    pub fn log_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if !self.identity.client_id.is_empty() {
            if let Ok(v) = HeaderValue::from_str(&self.identity.client_id) {
                headers.insert(HEADER_CLIENT, v);
            }
        }
        headers
    }
}

async fn parse_json<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, ZeroClawError> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp.json::<T>().await?)
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(ZeroClawError::Status { code: status, body })
    }
}

/// Percent-encode a single path segment (memory key etc.).
fn urlencode_path(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_derives_no_token() {
        let c = ZeroClawClient::new(
            "http://localhost:1234",
            "ws://localhost:1234",
            "",
            ClientIdentity::default(),
            "default",
            5_000,
        );
        assert_eq!(c.ws_url(), "ws://localhost:1234");
        assert!(!c.has_bearer());
        assert_eq!(c.agent_alias(), "default");
    }

    #[test]
    fn log_headers_includes_client_id() {
        let c = ZeroClawClient::new(
            "http://localhost:1234",
            "ws://localhost:1234",
            "",
            ClientIdentity {
                client_id: "jarvis-avatar".into(),
                webhook_secret: String::new(),
            },
            "default",
            5_000,
        );
        let h = c.log_headers();
        assert_eq!(h.get(HEADER_CLIENT).and_then(|v| v.to_str().ok()), Some("jarvis-avatar"));
    }
}
