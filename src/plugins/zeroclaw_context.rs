//! Bidirectional context pusher — writes avatar state into ZeroClaw's
//! `/api/memory` so the agent can recall pose, emotion, look-at target, and
//! the latest pose screenshot when answering chat.
//!
//! Keys (all under `[zeroclaw].memory_category`, default `"jarvis-avatar"`):
//!
//! | key                        | source |
//! |----------------------------|--------|
//! | `jarvis.emotion`           | ACT-parsed emotion from each [`ChatCompleteMessage`] |
//! | `jarvis.looking_at`        | Latest [`LookAtRequestMessage`] target |
//! | `jarvis.a2f_status`        | [`ServiceStatus`] entry for `A2fHealth` |
//! | `jarvis.tts_status`        | [`ServiceStatus`] entry for `Tts` |
//! | `jarvis.last_pose_view_url`| Periodic pose-capture screenshot URL |
//! | `jarvis.session_started`   | One-shot boot timestamp |
//!
//! Writes are throttled per-key by [`ZeroClawSettings::context_throttle_ms`]
//! and coalesced so a fast-moving look-at doesn't hammer the gateway. The
//! plugin no-ops when `gateway.backend != "zeroclaw"` or
//! `[zeroclaw].context_push_enabled = false`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use crossbeam_channel::{Sender, unbounded};

use crate::act::emotion_labels;
use crate::config::{ChatBackend, Settings};
use crate::zeroclaw::client::{ClientIdentity, ZeroClawClient};
use crate::zeroclaw::types::MemoryWriteRequest;

use super::channel_server::{ChatCompleteMessage, LookAtRequestMessage};
use super::pose_capture::{
    CaptureCameraOverrides, CaptureCommandSender, CaptureFramingPreset, CaptureRequest, CaptureView,
};
use super::service_status::{ServiceId, ServiceState, ServiceStatus};
use super::shared_runtime::SharedTokio;

const SESSION_KEY: &str = "jarvis.session_started";
const EMOTION_KEY: &str = "jarvis.emotion";
const LOOKING_AT_KEY: &str = "jarvis.looking_at";
const A2F_STATUS_KEY: &str = "jarvis.a2f_status";
const TTS_STATUS_KEY: &str = "jarvis.tts_status";
const POSE_VIEW_URL_KEY: &str = "jarvis.last_pose_view_url";

pub struct ZeroClawContextPlugin;

impl Plugin for ZeroClawContextPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, install_context_writer)
            .add_systems(
                Update,
                (
                    push_emotion_from_chat,
                    push_look_at_target,
                    push_service_status_snapshots,
                    pose_capture_tick,
                    process_pose_capture_replies,
                ),
            );
    }
}

// ---------- Boot --------------------------------------------------------------

#[derive(Resource, Clone)]
struct ContextWriter {
    tx: Sender<WriteJob>,
    category: String,
    throttle: Duration,
    /// Cached per-key (value, last_write) so we elide identical or fresh writes.
    last_written: Arc<std::sync::Mutex<HashMap<&'static str, (String, Instant)>>>,
}

#[derive(Resource, Default)]
struct PoseCaptureCadence {
    /// Wall-clock of the last successful capture trigger.
    last_triggered: Option<Instant>,
    /// Pending captures keyed by `capture_id` for url emission on completion.
    /// `crossbeam_channel::Receiver` is `Send + Sync`, so it survives the
    /// `Resource` trait bound that bare `std::sync::mpsc::Receiver` violates.
    pending: HashMap<String, crossbeam_channel::Receiver<super::pose_capture::CaptureResult>>,
}

struct WriteJob {
    request: MemoryWriteRequest,
}

fn install_context_writer(
    mut commands: Commands,
    settings: Res<Settings>,
    shared: Res<SharedTokio>,
) {
    if !matches!(
        ChatBackend::parse(&settings.gateway.backend),
        ChatBackend::Zeroclaw,
    ) {
        return;
    }
    let cfg = settings.zeroclaw.clone();
    if !cfg.context_push_enabled {
        info!("zeroclaw_context: disabled via config");
        return;
    }

    let identity = ClientIdentity {
        client_id: cfg.client_id.clone(),
        webhook_secret: cfg.webhook_secret.clone(),
    };
    let client = Arc::new(ZeroClawClient::new(
        cfg.normalized_base_url(),
        cfg.resolved_ws_url(),
        cfg.auth_token.clone(),
        identity,
        cfg.agent_alias.clone(),
        cfg.request_timeout_ms,
    ));

    let (tx, rx) = unbounded::<WriteJob>();
    let writer = ContextWriter {
        tx: tx.clone(),
        category: cfg.memory_category.clone(),
        throttle: Duration::from_millis(cfg.context_throttle_ms.max(100)),
        last_written: Arc::new(std::sync::Mutex::new(HashMap::new())),
    };

    // Spawn the writer task on the shared tokio runtime so we don't take a
    // dedicated thread just for occasional /api/memory POSTs.
    //
    // IMPORTANT: poll `try_recv` + `tokio::time::sleep` rather than the
    // blocking `rx.recv()`. crossbeam's `recv()` is sync-blocking, and a
    // tokio worker stuck inside it cannot be cancelled when the runtime
    // is shut down — that's what was freezing the app on close. The
    // chat-plugin worker uses the same pattern intentionally.
    let client_for_task = Arc::clone(&client);
    shared.spawn(async move {
        info!(
            "zeroclaw_context: writer task started against {}",
            client_for_task.base_url()
        );
        loop {
            match rx.try_recv() {
                Ok(job) => {
                    if let Err(e) = client_for_task.memory_write(&job.request).await {
                        warn!(
                            "zeroclaw_context: memory_write {:?} failed: {e}",
                            job.request.key
                        );
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    info!("zeroclaw_context: writer channel closed; stopping");
                    return;
                }
            }
        }
    });

    // One-shot session-start memory so the agent has a clear "the avatar
    // came online" marker without waiting for the first state change.
    let started = WriteJob {
        request: MemoryWriteRequest {
            key: SESSION_KEY.into(),
            content: format!(
                "Jarvis avatar online at {} (client_id={}, backend=zeroclaw)",
                chrono_timestamp(),
                cfg.client_id
            ),
            category: Some(cfg.memory_category.clone()),
        },
    };
    let _ = tx.send(started);

    commands.insert_resource(writer);
    commands.insert_resource(PoseCaptureCadence::default());
}

// ---------- Emotion from chat completions ------------------------------------

fn push_emotion_from_chat(
    mut events: MessageReader<ChatCompleteMessage>,
    writer: Option<Res<ContextWriter>>,
) {
    let Some(writer) = writer else {
        events.clear();
        return;
    };
    for ev in events.read() {
        let labels = emotion_labels(&ev.content);
        let Some(label) = labels.into_iter().next() else {
            continue;
        };
        writer.write_keyed(EMOTION_KEY, format!("Latest emotion: {label}"));
    }
}

// ---------- Look-at target ---------------------------------------------------

fn push_look_at_target(
    mut events: MessageReader<LookAtRequestMessage>,
    writer: Option<Res<ContextWriter>>,
) {
    let Some(writer) = writer else {
        events.clear();
        return;
    };
    let mut latest: Option<Option<Vec3>> = None;
    for ev in events.read() {
        latest = Some(ev.local_target);
    }
    let Some(target) = latest else {
        return;
    };
    let content = match target {
        None => "Looking at: (none — returning to idle)".to_string(),
        Some(v) => format!(
            "Looking at local target (x={:.2}, y={:.2}, z={:.2})",
            v.x, v.y, v.z
        ),
    };
    writer.write_keyed(LOOKING_AT_KEY, content);
}

// ---------- Service status mirror --------------------------------------------

fn push_service_status_snapshots(
    status: Option<Res<ServiceStatus>>,
    writer: Option<Res<ContextWriter>>,
) {
    let (Some(status), Some(writer)) = (status, writer) else {
        return;
    };
    if let Some(entry) = status.get(ServiceId::A2fHealth) {
        let line = format!(
            "A2F health: {} ({})",
            entry.state.short(),
            short(&entry.detail, 80)
        );
        writer.write_keyed(A2F_STATUS_KEY, line);
    }
    if let Some(entry) = status.get(ServiceId::Tts) {
        let line = format!(
            "Kokoro TTS: {} ({})",
            entry.state.short(),
            short(&entry.detail, 80)
        );
        writer.write_keyed(TTS_STATUS_KEY, line);
    }
    // Treat full-blown offline gateway state as worth signalling explicitly —
    // helps the agent reason about why a chat call may be slow or failing.
    if let Some(entry) = status.get(ServiceId::IronclawGateway) {
        if matches!(entry.state, ServiceState::Offline) {
            writer.write_keyed(
                "jarvis.ironclaw_gateway_state",
                format!("IronClaw gateway: offline ({})", short(&entry.detail, 80)),
            );
        }
    }
}

// ---------- Periodic pose capture --------------------------------------------

fn pose_capture_tick(
    settings: Res<Settings>,
    capture: Option<Res<CaptureCommandSender>>,
    writer: Option<Res<ContextWriter>>,
    cadence: Option<ResMut<PoseCaptureCadence>>,
) {
    let Some(capture) = capture else { return };
    let Some(_writer) = writer else { return };
    let Some(mut cadence) = cadence else { return };
    // Re-use the chat throttle as the floor here; capturing every second is
    // wasteful (and lights up the GPU pointlessly), so multiply by 30.
    let interval = Duration::from_millis(settings.zeroclaw.context_throttle_ms.max(1_000) * 30);
    let now = Instant::now();
    if let Some(last) = cadence.last_triggered {
        if now.duration_since(last) < interval {
            return;
        }
    }

    let output_dir = std::env::temp_dir().join("jarvis-zc-pose-views");
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        warn!("zeroclaw_context: pose view dir: {e}");
        return;
    }
    let capture_id = format!("zc-{}", uuid::Uuid::new_v4());
    // `pose_capture` already uses crossbeam channels; we reuse the same
    // primitive so the reply receiver is `Sync` and can live on a Bevy
    // `Resource` without an extra bridge thread.
    let (tx, rx) = crossbeam_channel::unbounded::<super::pose_capture::CaptureResult>();
    let req = CaptureRequest {
        output_dir: output_dir.clone(),
        capture_id: capture_id.clone(),
        width: 384,
        height: 512,
        views: vec![CaptureView::Front],
        framing_preset: Some(CaptureFramingPreset::FullBody),
        camera_overrides: Some(CaptureCameraOverrides {
            focus_y_offset: None,
            radius: None,
            height_lift: None,
        }),
        response_tx: tx,
    };
    if let Err(e) = capture.0.send(req) {
        warn!("zeroclaw_context: capture send failed: {e}");
        return;
    }
    cadence.last_triggered = Some(now);
    cadence.pending.insert(capture_id, rx);
}

fn process_pose_capture_replies(
    cadence: Option<ResMut<PoseCaptureCadence>>,
    writer: Option<Res<ContextWriter>>,
    settings: Res<Settings>,
) {
    let Some(mut cadence) = cadence else { return };
    let Some(writer) = writer else { return };
    let mut completed = Vec::new();
    for (id, rx) in cadence.pending.iter() {
        match rx.try_recv() {
            Ok(result) => completed.push((id.clone(), Some(result))),
            Err(crossbeam_channel::TryRecvError::Empty) => {}
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                completed.push((id.clone(), None));
            }
        }
    }
    for (id, maybe) in completed {
        cadence.pending.remove(&id);
        let Some(result) = maybe else {
            continue;
        };
        let Some(image) = result.images.first() else {
            continue;
        };
        // Try to expose the file via the attachments registry so the agent
        // can fetch it. `publish_images` works off in-memory base64; here we
        // have an on-disk file, so we serve it through the same registry by
        // reading + re-publishing. The double-copy is intentional: the
        // registry owns deletion lifecycle (sweep + max).
        let Ok(bytes) = std::fs::read(&image.path) else {
            continue;
        };
        let img = crate::ironclaw::types::ImageData {
            media_type: "image/png".to_string(),
            data: base64_encode(&bytes),
        };
        let Some(mut urls) = super::zeroclaw_attachments::publish_images(&[img]) else {
            continue;
        };
        let Some(url) = urls.pop() else {
            continue;
        };
        writer.write_keyed(
            POSE_VIEW_URL_KEY,
            format!(
                "Latest avatar pose view: {} (category={}, captured_at={})",
                url,
                settings.zeroclaw.memory_category,
                chrono_timestamp()
            ),
        );
    }
}

// ---------- Helpers -----------------------------------------------------------

impl ContextWriter {
    fn write_keyed(&self, key: &'static str, content: impl Into<String>) {
        let content = content.into();
        if let Ok(mut map) = self.last_written.lock() {
            let now = Instant::now();
            if let Some((prev_val, prev_at)) = map.get(key) {
                if prev_val == &content {
                    return;
                }
                if now.duration_since(*prev_at) < self.throttle {
                    return;
                }
            }
            map.insert(key, (content.clone(), now));
        }
        let req = MemoryWriteRequest {
            key: key.to_string(),
            content,
            category: Some(self.category.clone()),
        };
        let _ = self.tx.send(WriteJob { request: req });
    }
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Cheap monotonic timestamp suitable for free-form log messages. Avoids
/// pulling in `chrono`/`time` just for the context pusher.
fn chrono_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix_secs={now}")
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_truncates_with_ellipsis() {
        assert_eq!(short("hello", 10), "hello");
        let s = short("abcdefghij", 5);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= 5);
    }

    #[test]
    fn chrono_timestamp_is_nonempty() {
        let s = chrono_timestamp();
        assert!(s.starts_with("unix_secs="));
    }
}
