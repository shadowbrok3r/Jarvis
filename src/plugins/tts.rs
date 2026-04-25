//! Phase 4: Kokoro FastAPI → Bevy audio playback.
//!
//! Listens for [`TtsSpeakMessage`]. For each utterance we POST to `kokoro_url` on a background
//! tokio task (the HTTP client is `reqwest`), receive a **WAV** payload, then hand the bytes
//! back to Bevy via a crossbeam channel and feed them into `AudioSource` / `AudioPlayer`.
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

use std::io::Cursor;
use std::sync::Arc;
use std::thread;

use bevy::prelude::*;
use crossbeam_channel::{unbounded, Receiver, Sender};
use reqwest::Client;
use tokio::runtime::Builder;
use tokio::sync::mpsc;

use jarvis_avatar::config::Settings;

use super::channel_server::TtsSpeakMessage;
use super::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};

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
}

struct TtsReady {
    bytes: Arc<[u8]>,
    text_preview: String,
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
                    match fetch_wav(&client, &req).await {
                        Ok(bytes) => {
                            if let Some(ref log) = traffic {
                                log.push(
                                    TrafficChannel::TtsHttp,
                                    TrafficDirection::Inbound,
                                    format!("TTS response {} bytes WAV", bytes.len()),
                                    None,
                                );
                            }
                            info!(
                                "tts: received {} bytes for \"{}{}\"",
                                bytes.len(),
                                preview,
                                if req.text.len() > 60 { "…" } else { "" }
                            );
                            match validate_wav(&bytes) {
                                Ok(spec) => {
                                    info!(
                                        "tts: wav ok — {} Hz, {} ch, {} bit {:?}",
                                        spec.sample_rate,
                                        spec.channels,
                                        spec.bits_per_sample,
                                        spec.sample_format,
                                    );
                                    let _ = tx_ready.send(TtsReady {
                                        bytes,
                                        text_preview: preview,
                                    });
                                }
                                Err(e) => {
                                    warn!(
                                        "tts: dropping unplayable payload ({} bytes) for \"{}{}\" — {e}",
                                        bytes.len(),
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
    let reader = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|e| format!("hound: {e}"))?;
    let spec = reader.spec();
    // rodio's WAV decoder supports PCM 8/16/32 and IEEE_FLOAT 32. Reject
    // anything else up front so we never hand it to bevy_audio.
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 8 | 16 | 24 | 32) => Ok(spec),
        (hound::SampleFormat::Float, 32) => Ok(spec),
        (fmt, bits) => Err(format!("unsupported sample format: {fmt:?}, {bits} bit")),
    }
}

async fn fetch_wav(client: &Client, req: &TtsRequest) -> Result<Arc<[u8]>, String> {
    let endpoint = format!("{}/v1/audio/speech", req.url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": "kokoro",
        "voice": req.voice,
        "input": req.text,
        "response_format": "wav"
    });

    let resp = client
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("post {endpoint}: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(format!("kokoro {status}: {}", txt.chars().take(200).collect::<String>()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read bytes: {e}"))?;
    Ok(Arc::from(bytes.to_vec().into_boxed_slice()))
}

fn dispatch_tts_requests(
    settings: Res<Settings>,
    bridge: Option<Res<TtsBridge>>,
    mut reader: MessageReader<TtsSpeakMessage>,
) {
    let Some(bridge) = bridge else {
        return;
    };
    if !settings.tts.enabled {
        reader.clear();
        return;
    }
    for msg in reader.read() {
        if msg.text.trim().is_empty() {
            continue;
        }
        let req = TtsRequest {
            text: msg.text.clone(),
            voice: settings.tts.voice.clone(),
            url: settings.tts.kokoro_url.clone(),
        };
        if let Err(e) = bridge.tx_request.send(req) {
            warn!("tts: dispatch failed: {e}");
        }
    }
}

fn play_ready_clips(
    bridge: Option<Res<TtsBridge>>,
    mut sources: ResMut<Assets<bevy::audio::AudioSource>>,
    mut commands: Commands,
) {
    let Some(bridge) = bridge else {
        return;
    };
    while let Ok(ready) = bridge.rx_ready.try_recv() {
        let handle = sources.add(bevy::audio::AudioSource {
            bytes: ready.bytes,
        });
        commands.spawn((
            bevy::audio::AudioPlayer(handle),
            bevy::audio::PlaybackSettings::DESPAWN,
            TtsClip {
                preview: ready.text_preview,
            },
        ));
    }
}

#[derive(Component)]
struct TtsClip {
    #[allow(dead_code)]
    preview: String,
}
