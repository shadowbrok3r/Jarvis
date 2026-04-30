//! Phase 4: Kokoro FastAPI → Bevy audio playback.
//!
//! Listens for [`TtsSpeakMessage`]. For each utterance we POST to `kokoro_url` on a background
//! tokio task (the HTTP client is `reqwest`), receive a **WAV** payload, then hand the bytes
//! back to Bevy via a crossbeam channel and feed them into `AudioSource` / `AudioPlayer`.
//!
//! When `[a2f].enabled` and `[a2f].apply_from_tts` are true, the same validated WAV is decoded
//! to PCM16 and sent through NVIDIA A2F `ProcessAudioStream`; returned blendshape keyframes are
//! mapped to VRM expressions and queued as [`PoseCommand::AnimateExpressions`] on the main
//! thread alongside playback.
//!
//! Requires the `bevy/wav` feature (added in `Cargo.toml`).
//!
//! **Crash guard**: `bevy_audio 0.18`'s `play_queued_audio_system` calls
//! `rodio::Decoder::new(...).unwrap()` on whatever bytes land in an
//! `AudioSource`. If Kokoro returns a payload rodio can't decode (e.g. a
//! non-PCM WAV, a truncated response, or HTML/JSON error body the
//! `status.is_success()` check let through) the whole Bevy tick poisons and
//! we lose the window. We therefore pre-validate the bytes with `hound`
//! before shipping them to Bevy — if it won't parse, we drop the clip and
//! log instead of panicking.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::thread;

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use reqwest::Client;
use tokio::runtime::Builder;
use tokio::sync::mpsc;

use jarvis_avatar::a2f::{A2fClient, A2fConfig, A2fResult};
use jarvis_avatar::arkit::{ArkitKeyframe, map_keyframes_to_vrm};
use jarvis_avatar::config::Settings;

use super::chat_pipeline_status::{ChatPipelineStage, ChatPipelineStatus};
use super::pose_driver::{PoseCommand, PoseCommandSender};
use jarvis_avatar::kokoro_http::{
    fetch_kokoro_speech, pcm_s16le_mono_to_wav_bytes, wav_bytes_to_pcm16_mono,
};

use super::channel_server::TtsSpeakMessage;
use super::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};

/// Cap expression keyframes sent to [`PoseCommand::AnimateExpressions`] (MCP tool uses 256).
const MAX_A2F_CHAT_EXPR_KEYFRAMES: usize = 512;

pub struct TtsPlugin;

impl Plugin for TtsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_tts_thread)
            .add_systems(Update, (dispatch_tts_requests, play_ready_clips));
    }
}

#[derive(Resource)]
struct TtsBridge {
    /// Send `(text, voice, url)` into the HTTP thread.
    tx_request: Sender<TtsRequest>,
    /// Receive decoded `AudioSource` bytes back.
    rx_ready: Receiver<TtsReady>,
}

struct TtsRequest {
    text: String,
    voice: String,
    url: String,
    response_format: String,
    stream: bool,
    pcm_sample_rate: u32,
    /// Run A2F on the same audio and return a mapped expression clip for the main thread.
    a2f_apply: bool,
    a2f_cfg: A2fConfig,
}

struct TtsReady {
    bytes: Arc<[u8]>,
    text_preview: String,
    /// `(keyframes, duration_seconds)` for [`PoseCommand::AnimateExpressions`].
    face_clip: Option<(Vec<(f32, HashMap<String, f32>)>, f32)>,
}

fn subsample_expression_keyframes(
    frames: &[(f32, HashMap<String, f32>)],
    max_k: usize,
) -> Vec<(f32, HashMap<String, f32>)> {
    if frames.len() <= max_k {
        return frames.to_vec();
    }
    if max_k < 2 {
        return frames.to_vec();
    }
    let mut out = Vec::with_capacity(max_k);
    let last_i = frames.len() - 1;
    for i in 0..max_k {
        let idx = ((i as f64) * last_i as f64 / (max_k - 1) as f64).round() as usize;
        out.push(frames[idx].clone());
    }
    out
}

fn a2f_result_to_face_clip(result: &A2fResult) -> Option<(Vec<(f32, HashMap<String, f32>)>, f32)> {
    if result.keyframes.is_empty() {
        return None;
    }
    let arkit: Vec<ArkitKeyframe> = result
        .keyframes
        .iter()
        .map(|k| ArkitKeyframe {
            time_code: k.time_code,
            blend_shapes: k.blend_shapes.clone(),
        })
        .collect();
    let vrm = map_keyframes_to_vrm(&arkit, None);
    let mut frames: Vec<(f32, HashMap<String, f32>)> = vrm
        .iter()
        .map(|k| (k.time_code as f32, k.expressions.clone()))
        .collect();
    frames.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let max_t = frames.iter().map(|(t, _)| *t).fold(0.0f32, f32::max);
    let duration = max_t.max(0.05).min(120.0);
    frames = subsample_expression_keyframes(&frames, MAX_A2F_CHAT_EXPR_KEYFRAMES);
    if frames.is_empty() {
        return None;
    }
    Some((frames, duration))
}

fn spawn_tts_thread(mut commands: Commands, traffic: Option<Res<TrafficLogSink>>) {
    let (tx_request, rx_request) = unbounded::<TtsRequest>();
    let (tx_ready, rx_ready) = unbounded::<TtsReady>();
    let traffic = traffic.map(|t| (*t).clone());

    commands.insert_resource(TtsBridge {
        tx_request,
        rx_ready,
    });

    thread::Builder::new()
        .name("jarvis-tts".into())
        .spawn(move || {
            let rt = match Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    error!("tts runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                let client = Client::builder()
                    .timeout(std::time::Duration::from_secs(60))
                    .build()
                    .unwrap_or_else(|_| Client::new());

                // Promote the sync crossbeam receiver into an async-friendly mpsc so tokio::select
                // can await next requests without spinning.
                let (local_tx, mut local_rx) = mpsc::unbounded_channel::<TtsRequest>();
                let pump = thread::Builder::new()
                    .name("jarvis-tts-pump".into())
                    .spawn(move || {
                        while let Ok(req) = rx_request.recv() {
                            if local_tx.send(req).is_err() {
                                break;
                            }
                        }
                    })
                    .ok();

                while let Some(req) = local_rx.recv().await {
                    let preview = req.text.chars().take(60).collect::<String>();
                    if let Some(ref log) = traffic {
                        let endpoint = format!(
                            "{}/v1/audio/speech",
                            req.url.trim_end_matches('/')
                        );
                        log.push(
                            TrafficChannel::TtsHttp,
                            TrafficDirection::Outbound,
                            format!("POST {endpoint} (voice={})", req.voice),
                            Some(serde_json::json!({
                                "inputPreview": preview,
                            })),
                        );
                    }
                    match fetch_kokoro_speech(
                        &client,
                        &req.url,
                        &req.voice,
                        &req.text,
                        &req.response_format,
                        req.stream,
                    )
                    .await
                    {
                        Ok(bytes) => {
                            if let Some(ref log) = traffic {
                                log.push(
                                    TrafficChannel::TtsHttp,
                                    TrafficDirection::Inbound,
                                    format!(
                                        "TTS response {} bytes (format={})",
                                        bytes.len(),
                                        req.response_format
                                    ),
                                    None,
                                );
                            }
                            let input_len = bytes.len();
                            info!(
                                "tts: received {} bytes for \"{}{}\"",
                                input_len,
                                preview,
                                if req.text.len() > 60 { "…" } else { "" }
                            );
                            let wav_bytes: Result<Vec<u8>, String> =
                                if req.response_format.eq_ignore_ascii_case("pcm") {
                                    pcm_s16le_mono_to_wav_bytes(&bytes, req.pcm_sample_rate)
                                } else {
                                    Ok(bytes)
                                };
                            let wav_bytes = match wav_bytes {
                                Ok(b) => b,
                                Err(e) => {
                                    warn!(
                                        "tts: pcm→wav failed ({} bytes) for \"{}{}\" — {e}",
                                        input_len,
                                        preview,
                                        if req.text.len() > 60 { "…" } else { "" }
                                    );
                                    continue;
                                }
                            };
                            match validate_wav(wav_bytes.as_slice()) {
                                Ok(spec) => {
                                    info!(
                                        "tts: wav ok — {} Hz, {} ch, {} bit {:?}",
                                        spec.sample_rate,
                                        spec.channels,
                                        spec.bits_per_sample,
                                        spec.sample_format,
                                    );
                                    let mut face_clip = None;
                                    if req.a2f_apply && req.a2f_cfg.enabled {
                                        match wav_bytes_to_pcm16_mono(&wav_bytes) {
                                            Ok((pcm, rate)) => {
                                                match A2fClient::new(req.a2f_cfg.clone())
                                                    .process_audio_pcm16(pcm, rate, None)
                                                    .await
                                                {
                                                    Ok(result) => {
                                                        if let Some(ref log) = traffic {
                                                            log.push(
                                                                TrafficChannel::A2fGrpc,
                                                                TrafficDirection::Outbound,
                                                                format!(
                                                                    "chat TTS → A2F ({} keyframes)",
                                                                    result.keyframes.len()
                                                                ),
                                                                None,
                                                            );
                                                        }
                                                        face_clip = a2f_result_to_face_clip(&result);
                                                        if face_clip.is_none() {
                                                            warn!(
                                                                "tts: A2F returned no mappable expression keyframes for \"{}{}\"",
                                                                preview,
                                                                if req.text.len() > 60 { "…" } else { "" }
                                                            );
                                                        }
                                                    }
                                                    Err(e) => {
                                                        warn!(
                                                            "tts: A2F after Kokoro failed for \"{}{}\": {e}",
                                                            preview,
                                                            if req.text.len() > 60 { "…" } else { "" }
                                                        );
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                warn!("tts: wav→pcm for A2F failed: {e}");
                                            }
                                        }
                                    }
                                    let _ = tx_ready.send(TtsReady {
                                        bytes: Arc::from(wav_bytes.into_boxed_slice()),
                                        text_preview: preview,
                                        face_clip,
                                    });
                                }
                                Err(e) => {
                                    warn!(
                                        "tts: dropping unplayable payload ({} bytes) for \"{}{}\" — {e}",
                                        wav_bytes.len(),
                                        preview,
                                        if req.text.len() > 60 { "…" } else { "" }
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            if let Some(ref log) = traffic {
                                log.push(
                                    TrafficChannel::TtsHttp,
                                    TrafficDirection::Inbound,
                                    format!("TTS error: {e}"),
                                    None,
                                );
                            }
                            warn!("tts: kokoro error: {e}");
                        }
                    }
                }

                drop(pump);
            });
        })
        .expect("failed to spawn jarvis-tts thread");
}

/// Parse the RIFF/WAVE header with `hound`. Returns the declared spec on
/// success, or an error string explaining why the payload is unplayable.
///
/// Catches every failure mode we've seen from Kokoro in the wild:
/// * HTML / JSON error body served with `200 OK` (no RIFF magic).
/// * Truncated stream (RIFF header but declared size larger than payload).
/// * Non-PCM, non-IEEE_FLOAT formats rodio's WAV decoder rejects.
fn validate_wav(bytes: &[u8]) -> Result<hound::WavSpec, String> {
    // `hound` is stricter than rodio in one direction (no A-law / μ-law) and
    // strictly less strict in the other (it accepts 24-bit, rodio doesn't).
    // Check the header first so common bogus payloads fail fast; then
    // hand-off to `hound` for the rest.
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!(
            "not a WAV (first bytes: {:02x?})",
            &bytes[..bytes.len().min(12)]
        ));
    }
    let reader = hound::WavReader::new(Cursor::new(bytes)).map_err(|e| format!("hound: {e}"))?;
    let spec = reader.spec();
    // rodio's WAV decoder supports PCM 8/16/32 and IEEE_FLOAT 32. Reject
    // anything else up front so we never hand it to bevy_audio.
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 8 | 16 | 24 | 32) => Ok(spec),
        (hound::SampleFormat::Float, 32) => Ok(spec),
        (fmt, bits) => Err(format!("unsupported sample format: {fmt:?}, {bits} bit")),
    }
}

fn a2f_config_from_settings(s: &Settings) -> A2fConfig {
    A2fConfig {
        enabled: s.a2f.enabled,
        endpoint: s.a2f.endpoint.clone(),
        health_url: s.a2f.health_url.clone(),
        function_id: s.a2f.function_id.clone(),
    }
}

fn dispatch_tts_requests(
    settings: Res<Settings>,
    bridge: Option<Res<TtsBridge>>,
    mut reader: MessageReader<TtsSpeakMessage>,
    mut pipeline: ResMut<ChatPipelineStatus>,
) {
    let Some(bridge) = bridge else {
        return;
    };
    if !settings.tts.enabled {
        reader.clear();
        return;
    }
    let a2f_cfg = a2f_config_from_settings(&settings);
    let a2f_apply = settings.a2f.enabled
        && settings.a2f.apply_from_tts
        && !settings.a2f.endpoint.trim().is_empty();
    for msg in reader.read() {
        if msg.text.trim().is_empty() {
            continue;
        }
        let req = TtsRequest {
            text: msg.text.clone(),
            voice: settings.tts.voice.clone(),
            url: settings.tts.kokoro_url.clone(),
            response_format: settings.tts.response_format.clone(),
            stream: settings.tts.stream,
            pcm_sample_rate: settings.tts.pcm_sample_rate,
            a2f_apply,
            a2f_cfg: a2f_cfg.clone(),
        };
        pipeline.set(
            ChatPipelineStage::KokoroSynthesizing,
            format!(
                "POST Kokoro ({}){}",
                settings.tts.response_format,
                if a2f_apply { " + A2F" } else { "" }
            ),
        );
        if let Err(e) = bridge.tx_request.send(req) {
            warn!("tts: dispatch failed: {e}");
            pipeline.set(ChatPipelineStage::Idle, "TTS dispatch failed".to_string());
        }
    }
}

fn play_ready_clips(
    bridge: Option<Res<TtsBridge>>,
    mut sources: ResMut<Assets<bevy::audio::AudioSource>>,
    mut commands: Commands,
    mut pipeline: ResMut<ChatPipelineStatus>,
    pose_tx: Option<Res<PoseCommandSender>>,
) {
    let Some(bridge) = bridge else {
        return;
    };
    let mut n = 0u32;
    let mut any_face_this_frame = false;
    let mut last_clip_drove_face = false;
    while let Ok(ready) = bridge.rx_ready.try_recv() {
        n += 1;
        let handle = sources.add(bevy::audio::AudioSource { bytes: ready.bytes });
        commands.spawn((
            bevy::audio::AudioPlayer(handle),
            bevy::audio::PlaybackSettings::DESPAWN,
            TtsClip {
                preview: ready.text_preview.clone(),
            },
        ));
        last_clip_drove_face = false;
        if let Some((keyframes, duration_seconds)) = ready.face_clip {
            if let Some(tx) = pose_tx.as_ref() {
                let kf_n = keyframes.len();
                tx.send(PoseCommand::AnimateExpressions {
                    keyframes,
                    duration_seconds,
                    looping: false,
                });
                pipeline.set(
                    ChatPipelineStage::A2fLipSync,
                    format!("{kf_n} expr keys · {duration_seconds:.2}s"),
                );
                last_clip_drove_face = true;
                any_face_this_frame = true;
            } else {
                warn!(
                    "tts: A2F face clip ready but PoseCommandSender missing — load PoseDriverPlugin"
                );
            }
        }
    }
    if n == 0 {
        return;
    }
    if !any_face_this_frame {
        pipeline.set(
            ChatPipelineStage::KokoroPlaying,
            if n == 1 {
                "Bevy audio clip".into()
            } else {
                format!("Bevy audio clips ×{n}")
            },
        );
    } else if !last_clip_drove_face {
        // Rare: A2F clip then extra audio-only clips in the same drain — show playback.
        pipeline.set(
            ChatPipelineStage::KokoroPlaying,
            format!("Bevy audio clips ×{n} (after A2F)"),
        );
    }
}

#[derive(Component)]
struct TtsClip {
    #[allow(dead_code)]
    preview: String,
}
