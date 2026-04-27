//! Kimodo motion-generation client.
//!
//! Kimodo itself is an external Python service (`kimodo-motion-service.py`)
//! that runs as a *peer* on our channel hub — it connects to `ws://hub:6121/ws`,
//! authenticates, and waits for `kimodo:generate` / `kimodo:list-animations`
//! envelopes. Status updates come back as `kimodo:status` and generated
//! motion is streamed as `vrm:apply-pose` envelopes.
//!
//! This module is the **hub-side** client: the RMCP tool handlers call into
//! [`KimodoClient`], which just publishes the right envelope through
//! [`HubBroadcast`] and (for `generate_motion`) awaits the matching
//! `kimodo:status` using the broadcast subscription on incoming envelopes.
//!
//! Correlation is done via `metadata.event.id`. Kimodo echoes it back as
//! `data.requestId`, so we pin the outbound id with
//! [`HubBroadcast::send_with_event_id`] and filter the inbound stream by it.
//!
//! `list_generated_animations` and the on-disk CRUD (rename / delete / update
//! metadata) don't need a round trip — Kimodo and `jarvis-avatar` share the
//! same animations directory, so we read / mutate it directly through
//! [`PoseLibrary`].

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use jarvis_avatar::ironclaw::protocol::EnvelopeBody;
use jarvis_avatar::pose_library::{AnimationFrame, BoneRotation};

use crate::plugins::channel_server::HubBroadcast;
use crate::plugins::native_anim_player::StreamingAnimation;

// `HubBroadcast` contains a `Sender` which isn't `Debug`; keep the client
// itself `Debug`-printable without pulling the full hub into the output.

/// Public-facing result of a `kimodo:generate` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KimodoGenerateOutcome {
    pub request_id: String,
    pub prompt: String,
    pub duration: f32,
    pub steps: u32,
    pub streamed: bool,
    pub save_name: Option<String>,
    /// Final status line from Kimodo (`"done"`, `"error"`, `"ready"`, …).
    pub final_status: String,
    /// Human-readable message that accompanied the final status.
    pub final_message: String,
    /// Populated when Kimodo reports it has produced frames.
    pub frame_count: Option<u32>,
    pub fps: Option<f32>,
    /// If Kimodo included `savedPath` on the final status (on-disk VRM JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
}

/// Tunables for a single generate request.
#[derive(Debug, Clone)]
pub struct GenerateRequest {
    pub prompt: String,
    pub duration: f32,
    pub steps: u32,
    /// When `true`, Kimodo will also emit `vrm:apply-pose` frames in real time.
    pub stream: bool,
    pub save_name: Option<String>,
    /// How long to wait for the final `kimodo:status` before giving up. A
    /// generation can easily take 20-60s on a mid-range GPU so the default is
    /// deliberately generous.
    pub timeout: Duration,
}

impl Default for GenerateRequest {
    fn default() -> Self {
        Self {
            prompt: "A person stands still".to_string(),
            duration: 3.0,
            steps: 100,
            stream: true,
            save_name: None,
            timeout: Duration::from_secs(180),
        }
    }
}

/// Thin shim over [`HubBroadcast`].
#[derive(Clone)]
pub struct KimodoClient {
    hub: HubBroadcast,
    /// Optional Bevy-side streaming lane — when set, incoming
    /// `vrm:apply-pose` frames during a streaming generate are also pushed
    /// into this shared ring buffer so the native player can drive the
    /// avatar on Bevy's clock (rather than whatever rate Kimodo emits).
    streaming: Option<StreamingAnimation>,
}

impl std::fmt::Debug for KimodoClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KimodoClient").finish_non_exhaustive()
    }
}

impl KimodoClient {
    pub fn new(hub: HubBroadcast) -> Self {
        Self {
            hub,
            streaming: None,
        }
    }

    pub fn with_streaming(mut self, streaming: StreamingAnimation) -> Self {
        self.streaming = Some(streaming);
        self
    }

    /// Ask Kimodo to generate motion for `request.prompt`. Returns once Kimodo
    /// emits a terminal `kimodo:status` (`"done"` / `"error"` / `"ready"` when
    /// not streaming), or `timeout` elapses.
    ///
    /// While this is awaiting, Kimodo's `vrm:apply-pose` frames are landing on
    /// the hub and driving the avatar directly — this call does not need to
    /// (and does not) forward them itself.
    pub async fn generate_motion(
        &self,
        request: GenerateRequest,
    ) -> Result<KimodoGenerateOutcome, KimodoError> {
        let request_id = Uuid::new_v4().to_string();
        let mut rx = self.hub.subscribe_incoming();

        let payload = json!({
            "prompt": request.prompt,
            "duration": request.duration,
            "steps": request.steps,
            "stream": request.stream,
            "saveName": request.save_name,
        });
        self.hub
            .send_with_event_id("kimodo:generate", payload, request_id.clone());

        if let Some(streaming) = self.streaming.as_ref() {
            // Default streaming fps; will be overwritten by kimodo:status when known.
            streaming.begin(request_id.clone(), 30.0);
        }

        let deadline = tokio::time::Instant::now() + request.timeout;
        let mut last_message = String::new();
        let mut frame_count: Option<u32> = None;
        let mut fps: Option<f32> = None;
        let mut saved_path: Option<String> = None;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                if let Some(streaming) = self.streaming.as_ref() {
                    streaming.end();
                }
                return Err(KimodoError::Timeout {
                    request_id,
                    waited: request.timeout,
                });
            }

            let env = match tokio::time::timeout(remaining, async { rx.recv().await }).await {
                Ok(Ok(env)) => env,
                Ok(Err(RecvError::Closed)) => {
                    if let Some(streaming) = self.streaming.as_ref() {
                        streaming.end();
                    }
                    return Err(KimodoError::HubClosed);
                }
                Ok(Err(RecvError::Lagged(_))) => continue,
                Err(_) => {
                    if let Some(streaming) = self.streaming.as_ref() {
                        streaming.end();
                    }
                    return Err(KimodoError::Timeout {
                        request_id,
                        waited: request.timeout,
                    });
                }
            };

            // Any `vrm:apply-pose` envelope during a streaming generate —
            // regardless of whose request_id it originated from — is kept
            // inside the streaming ring buffer. Kimodo is the only sane
            // source for streamed frames; we don't further filter here so
            // consumers don't need to thread a request_id through.
            if request.stream
                && env.message_type == "vrm:apply-pose"
                && self.streaming.is_some()
            {
                if let Some(streaming) = self.streaming.as_ref() {
                    if let Some(frame) = parse_apply_pose_frame(&env) {
                        streaming.push_frame(frame);
                    }
                }
            }

            if !kimodo_reply_for(&env, &request_id) {
                continue;
            }

            match env.message_type.as_str() {
                "kimodo:status" => {
                    let status = env
                        .data
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    last_message = env
                        .data
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();

                    if let Some(fc) = env.data.get("frameCount").and_then(Value::as_u64) {
                        frame_count = Some(fc as u32);
                    }
                    if let Some(f) = env.data.get("fps").and_then(Value::as_f64) {
                        fps = Some(f as f32);
                        // Sync streaming lane's fps so native playback matches.
                        if let Some(streaming) = self.streaming.as_ref() {
                            if streaming.active_request_id().as_deref() == Some(request_id.as_str()) {
                                streaming.begin(request_id.clone(), f as f32);
                            }
                        }
                    }
                    if let Some(p) = env
                        .data
                        .get("savedPath")
                        .or_else(|| env.data.get("saved_path"))
                        .and_then(Value::as_str)
                    {
                        if !p.is_empty() {
                            saved_path = Some(p.to_string());
                        }
                    }

                    match status.as_str() {
                        "done" | "error" => {
                            if let Some(streaming) = self.streaming.as_ref() {
                                streaming.end();
                            }
                            return Ok(KimodoGenerateOutcome {
                                request_id,
                                prompt: request.prompt,
                                duration: request.duration,
                                steps: request.steps,
                                streamed: request.stream,
                                save_name: request.save_name,
                                final_status: status,
                                final_message: last_message,
                                frame_count,
                                fps,
                                saved_path: saved_path.clone(),
                            });
                        }
                        "ready" if !request.stream => {
                            if let Some(streaming) = self.streaming.as_ref() {
                                streaming.end();
                            }
                            // Non-streaming path is terminal at "ready"; the
                            // result envelope (`kimodo:generate:result`) follows
                            // but we don't need it to return a summary.
                            return Ok(KimodoGenerateOutcome {
                                request_id,
                                prompt: request.prompt,
                                duration: request.duration,
                                steps: request.steps,
                                streamed: request.stream,
                                save_name: request.save_name,
                                final_status: "ready".to_string(),
                                final_message: last_message,
                                frame_count,
                                fps,
                                saved_path: saved_path.clone(),
                            });
                        }
                        _ => {
                            // "generating" / "streaming" / other progress markers — keep waiting.
                        }
                    }
                }
                "kimodo:generate:result" if !request.stream => {
                    if let Some(streaming) = self.streaming.as_ref() {
                        streaming.end();
                    }
                    if frame_count.is_none() {
                        if let Some(fc) = env.data.get("frameCount").and_then(Value::as_u64) {
                            frame_count = Some(fc as u32);
                        }
                    }
                    if fps.is_none() {
                        if let Some(f) = env.data.get("fps").and_then(Value::as_f64) {
                            fps = Some(f as f32);
                        }
                    }
                    return Ok(KimodoGenerateOutcome {
                        request_id,
                        prompt: request.prompt,
                        duration: request.duration,
                        steps: request.steps,
                        streamed: request.stream,
                        save_name: request.save_name,
                        final_status: "ready".to_string(),
                        final_message: last_message,
                        frame_count,
                        fps,
                        saved_path: saved_path.clone(),
                    });
                }
                _ => {}
            }
        }
    }

    /// Ask Kimodo to replay a previously-saved animation by filename. Returns as
    /// soon as Kimodo acknowledges with `kimodo:status`. Playback itself happens
    /// via `vrm:apply-pose` frames that go directly into the hub / Bevy.
    pub fn play_saved_animation(&self, filename: impl Into<String>) -> String {
        let request_id = Uuid::new_v4().to_string();
        self.hub.send_with_event_id(
            "kimodo:play-animation",
            json!({ "filename": filename.into() }),
            request_id.clone(),
        );
        request_id
    }
}

fn parse_apply_pose_frame(env: &EnvelopeBody) -> Option<AnimationFrame> {
    let bones_obj = env.data.get("bones").and_then(Value::as_object)?;
    let mut bones: HashMap<String, BoneRotation> = HashMap::new();
    for (name, entry) in bones_obj {
        let rotation = if let Some(arr) = entry.get("rotation").and_then(Value::as_array) {
            arr
        } else if let Some(arr) = entry.as_array() {
            arr
        } else {
            continue;
        };
        if rotation.len() != 4 {
            continue;
        }
        let mut q = [0.0f32; 4];
        let mut ok = true;
        for (i, v) in rotation.iter().enumerate() {
            match v.as_f64() {
                Some(n) => q[i] = n as f32,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        bones.insert(name.clone(), BoneRotation { rotation: q });
    }
    if bones.is_empty() {
        return None;
    }
    Some(AnimationFrame {
        bones,
        duration_ms: None,
    })
}

/// Does `env` look like a reply to our `request_id`?
///
/// Kimodo echoes `request_id` back in **either** `data.requestId` **or**
/// `metadata.event.id`, depending on the message, so we check both.
fn kimodo_reply_for(env: &EnvelopeBody, request_id: &str) -> bool {
    if let Some(rid) = env.data.get("requestId").and_then(Value::as_str) {
        if rid == request_id {
            return true;
        }
    }
    if let Some(rid) = env
        .metadata
        .get("event")
        .and_then(|e| e.get("id"))
        .and_then(Value::as_str)
    {
        if rid == request_id {
            return true;
        }
    }
    false
}

#[derive(Debug, thiserror::Error)]
pub enum KimodoError {
    #[error("timed out after {waited:?} waiting for kimodo reply to {request_id}")]
    Timeout {
        request_id: String,
        waited: Duration,
    },
    #[error("channel hub shut down before kimodo replied")]
    HubClosed,
}
