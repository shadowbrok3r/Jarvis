//! NVIDIA Audio2Face-3D gRPC client + health check.
//!
//! Mirrors the behaviour of the Node `a2f-client.mjs`:
//!
//! * **Health** — plain HTTP GET on `health_url`, treated healthy iff JSON body
//!   carries `status == "ready"`.
//! * **Processing** — bidirectional-streaming RPC `ProcessAudioStream`: we send
//!   `AudioStreamHeader` once, then 1-second `AudioWithEmotion` chunks, then
//!   `EndOfAudio`, and collect blendshape keyframes from the server's reply.
//!
//! The defaults for `face_params`, `blendshape_params`, and the emotion
//! post-processing block are identical to the JS reference so operators get
//! the same-feeling animation after porting.

pub mod pb;

use std::collections::HashMap;
use std::time::Duration;

use async_stream::stream;
use pb::a2f::v1 as a2f_pb;
use pb::audio::v1 as audio_pb;
use pb::controller::v1 as ctrl_pb;
use pb::emotion_with_timecode::v1 as emo_pb;
use pb::services::a2f_controller::v1::a2f_controller_service_client::A2fControllerServiceClient;
use serde::{Deserialize, Serialize};
use tonic::transport::Endpoint;

/// Configurable endpoints — wired from [`crate::config::A2fSettings`] by callers.
#[derive(Debug, Clone)]
pub struct A2fConfig {
    /// Feature flag — keep `process_audio_pcm16` no-oppable without throwing.
    pub enabled: bool,
    /// gRPC endpoint, e.g. `http://localhost:52000`.
    pub endpoint: String,
    /// HTTP health URL, e.g. `http://localhost:8000/v1/health/ready`.
    pub health_url: String,
    /// Same UUID as the A2F service `--function-id` (avatar / function binding). Carried for config
    /// snapshots and any future stream wiring; the controller `AudioStreamHeader` proto has no ID slot.
    pub function_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HealthStatus {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One PCM frame of animation output from A2F.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2fKeyframe {
    pub time_code: f64,
    pub blend_shapes: HashMap<String, f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct A2fResult {
    pub keyframes: Vec<A2fKeyframe>,
    pub blend_shape_names: Vec<String>,
    /// Emotion dictionary actually sent on the first `AudioWithEmotion` chunk (A2F emotion keys:
    /// `joy`, `anger`, …). The gRPC stream does not currently return per-frame emotion envelopes in
    /// our parser; TTS uses this for [`crate::arkit::merge_a2f_emotion_hint_into_keyframes`].
    #[serde(default)]
    pub emotion_hints_applied: HashMap<String, f32>,
}

#[derive(Debug, thiserror::Error)]
pub enum A2fError {
    #[error("invalid endpoint: {0}")]
    Endpoint(String),
    #[error("gRPC transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("gRPC status: {0}")]
    Status(#[from] tonic::Status),
}

/// Default face parameters (straight port of `DEFAULT_FACE_PARAMS` in `a2f-client.mjs`).
pub fn default_face_params() -> a2f_pb::FaceParameters {
    let mut float_params = HashMap::new();
    for (k, v) in [
        ("upperFaceStrength", 1.0_f32),
        ("upperFaceSmoothing", 0.001),
        ("lowerFaceStrength", 1.25),
        ("lowerFaceSmoothing", 0.006),
        ("faceMaskLevel", 0.6),
        ("faceMaskSoftness", 0.0085),
        ("skinStrength", 1.0),
        ("eyelidOpenOffset", 0.0),
        ("lipOpenOffset", 0.0),
        ("tongueStrength", 1.3),
        ("tongueHeightOffset", 0.0),
        ("tongueDepthOffset", 0.0),
    ] {
        float_params.insert(k.to_string(), v);
    }
    a2f_pb::FaceParameters {
        float_params,
        integer_params: HashMap::new(),
        float_array_params: HashMap::new(),
    }
}

pub fn default_emotion_pp() -> a2f_pb::EmotionPostProcessingParameters {
    a2f_pb::EmotionPostProcessingParameters {
        emotion_contrast: Some(1.0),
        live_blend_coef: Some(0.7),
        enable_preferred_emotion: Some(false),
        preferred_emotion_strength: Some(0.5),
        emotion_strength: Some(0.6),
        max_emotions: Some(3),
    }
}

pub fn default_blendshape_params() -> a2f_pb::BlendShapeParameters {
    let multipliers = [
        ("EyeBlinkLeft", 1.0),
        ("EyeSquintLeft", 1.0),
        ("EyeWideLeft", 1.0),
        ("EyeBlinkRight", 1.0),
        ("EyeSquintRight", 1.0),
        ("EyeWideRight", 1.0),
        ("EyeLookDownLeft", 0.0),
        ("EyeLookInLeft", 0.0),
        ("EyeLookOutLeft", 0.0),
        ("EyeLookUpLeft", 0.0),
        ("EyeLookDownRight", 0.0),
        ("EyeLookInRight", 0.0),
        ("EyeLookOutRight", 0.0),
        ("EyeLookUpRight", 0.0),
        ("JawForward", 0.7),
        ("JawLeft", 0.2),
        ("JawRight", 0.2),
        ("JawOpen", 1.0),
        ("MouthClose", 1.0),
        ("MouthFunnel", 1.2),
        ("MouthPucker", 1.2),
        ("MouthLeft", 0.2),
        ("MouthRight", 0.2),
        ("MouthSmileLeft", 0.8),
        ("MouthSmileRight", 0.8),
        ("MouthFrownLeft", 0.4),
        ("MouthFrownRight", 0.4),
        ("MouthDimpleLeft", 0.7),
        ("MouthDimpleRight", 0.7),
        ("MouthStretchLeft", 0.1),
        ("MouthStretchRight", 0.1),
        ("MouthRollLower", 0.9),
        ("MouthRollUpper", 0.5),
        ("MouthShrugLower", 0.9),
        ("MouthShrugUpper", 0.4),
        ("MouthPressLeft", 0.8),
        ("MouthPressRight", 0.8),
        ("MouthLowerDownLeft", 0.8),
        ("MouthLowerDownRight", 0.8),
        ("MouthUpperUpLeft", 0.8),
        ("MouthUpperUpRight", 0.8),
        ("BrowDownLeft", 1.0),
        ("BrowDownRight", 1.0),
        ("BrowInnerUp", 1.0),
        ("BrowOuterUpLeft", 1.0),
        ("BrowOuterUpRight", 1.0),
        ("CheekPuff", 0.2),
        ("CheekSquintLeft", 1.0),
        ("CheekSquintRight", 1.0),
        ("NoseSneerLeft", 0.8),
        ("NoseSneerRight", 0.8),
        ("TongueOut", 0.0),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();

    a2f_pb::BlendShapeParameters {
        bs_weight_multipliers: multipliers,
        bs_weight_offsets: HashMap::new(),
        enable_clamping_bs_weight: Some(true),
    }
}

/// One-shot A2F client. `endpoint` may be `localhost:52000` (schemeless) or
/// `http://localhost:52000`; we normalise to the former if needed.
#[derive(Debug, Clone)]
pub struct A2fClient {
    cfg: A2fConfig,
    http: reqwest::Client,
}

impl A2fClient {
    pub fn new(cfg: A2fConfig) -> Self {
        Self {
            cfg,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap_or_default(),
        }
    }

    pub fn config(&self) -> &A2fConfig {
        &self.cfg
    }

    /// HTTP health ping. Returns `ok=true` iff server reports `status == "ready"`.
    pub async fn health(&self) -> HealthStatus {
        #[derive(Deserialize)]
        struct Ready {
            status: Option<String>,
        }
        match self.http.get(&self.cfg.health_url).send().await {
            Ok(resp) => match resp.json::<Ready>().await {
                Ok(body) => HealthStatus {
                    ok: body.status.as_deref() == Some("ready"),
                    status: body.status,
                    error: None,
                },
                Err(e) => HealthStatus {
                    ok: false,
                    status: None,
                    error: Some(e.to_string()),
                },
            },
            Err(e) => HealthStatus {
                ok: false,
                status: None,
                error: Some(e.to_string()),
            },
        }
    }

    /// Send PCM-16 mono audio and collect blendshape keyframes.
    ///
    /// `emotion_hints` gets attached as a single emotion timecode at `t=0`;
    /// pass `None` to default to `{joy: 0.5}` like the JS client.
    pub async fn process_audio_pcm16(
        &self,
        pcm: Vec<u8>,
        sample_rate: u32,
        emotion_hints: Option<HashMap<String, f32>>,
    ) -> Result<A2fResult, A2fError> {
        let ep = normalize_endpoint(&self.cfg.endpoint);
        let endpoint = Endpoint::from_shared(ep).map_err(|e| A2fError::Endpoint(e.to_string()))?;
        let channel = endpoint.connect().await?;
        let mut client = A2fControllerServiceClient::new(channel);

        let header = ctrl_pb::AudioStreamHeader {
            audio_header: Some(audio_pb::AudioHeader {
                audio_format: audio_pb::audio_header::AudioFormat::Pcm as i32,
                channel_count: 1,
                samples_per_second: sample_rate,
                bits_per_sample: 16,
            }),
            face_params: Some(default_face_params()),
            emotion_post_processing_params: Some(default_emotion_pp()),
            blendshape_params: Some(default_blendshape_params()),
            emotion_params: None,
        };

        let emotion_hints_applied = emotion_hints.unwrap_or_else(|| {
            let mut m = HashMap::new();
            m.insert("joy".to_string(), 0.5);
            m
        });
        let emotions_first = vec![emo_pb::EmotionWithTimeCode {
            time_code: 0.0,
            emotion: emotion_hints_applied.clone(),
        }];

        let bytes_per_second = (sample_rate as usize) * 2;
        let outbound = stream! {
            yield ctrl_pb::AudioStream {
                stream_part: Some(ctrl_pb::audio_stream::StreamPart::AudioStreamHeader(header)),
            };
            let mut start = 0usize;
            let mut first = true;
            while start < pcm.len() {
                let end = (start + bytes_per_second).min(pcm.len());
                let chunk = pcm[start..end].to_vec();
                let awe = a2f_pb::AudioWithEmotion {
                    audio_buffer: chunk,
                    emotions: if first { emotions_first.clone() } else { Vec::new() },
                };
                first = false;
                start = end;
                yield ctrl_pb::AudioStream {
                    stream_part: Some(ctrl_pb::audio_stream::StreamPart::AudioWithEmotion(awe)),
                };
            }
            yield ctrl_pb::AudioStream {
                stream_part: Some(ctrl_pb::audio_stream::StreamPart::EndOfAudio(
                    ctrl_pb::audio_stream::EndOfAudio {},
                )),
            };
        };

        let response = client.process_audio_stream(outbound).await?;
        let mut inbound = response.into_inner();

        let mut blend_shape_names: Vec<String> = Vec::new();
        let mut keyframes: Vec<A2fKeyframe> = Vec::new();

        while let Some(msg) = inbound.message().await? {
            let Some(part) = msg.stream_part else { continue };
            match part {
                ctrl_pb::animation_data_stream::StreamPart::AnimationDataStreamHeader(h) => {
                    if let Some(skel) = h.skel_animation_header {
                        blend_shape_names = skel.blend_shapes;
                    }
                }
                ctrl_pb::animation_data_stream::StreamPart::AnimationData(d) => {
                    let Some(skel) = d.skel_animation else { continue };
                    for frame in skel.blend_shape_weights {
                        let mut bs = HashMap::with_capacity(blend_shape_names.len());
                        for (i, name) in blend_shape_names.iter().enumerate() {
                            if let Some(v) = frame.values.get(i).copied() {
                                bs.insert(name.clone(), v);
                            }
                        }
                        keyframes.push(A2fKeyframe {
                            time_code: frame.time_code,
                            blend_shapes: bs,
                        });
                    }
                }
                ctrl_pb::animation_data_stream::StreamPart::Event(_) => {}
                ctrl_pb::animation_data_stream::StreamPart::Status(s) => {
                    if s.code != 0 {
                        tracing::warn!(code = s.code, msg = %s.message, "a2f non-success status");
                    }
                }
            }
        }

        Ok(A2fResult {
            keyframes,
            blend_shape_names,
            emotion_hints_applied,
        })
    }
}

fn normalize_endpoint(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    }
}
