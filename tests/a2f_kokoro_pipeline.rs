//! End-to-end Kokoro → WAV → PCM16 → Audio2Face (ignored unless env is set).
//!
//! Run on a machine that can reach your services, for example:
//!
//! ```text
//! export JARVIS_KOKORO_URL=http://192.168.4.9:8880
//! export JARVIS_A2F_ENDPOINT=http://192.168.4.9:52000
//! export JARVIS_A2F_HEALTH_URL=http://192.168.4.9:8000/v1/health/ready
//! export JARVIS_A2F_FUNCTION_ID=0961a6da-fb9e-4f2e-8491-247e5fd7bf8d   # optional; Claire default in test
//! export JARVIS_TTS_VOICE=af_heart
//! cargo test -p jarvis-avatar --test a2f_kokoro_pipeline -- --ignored --nocapture
//! ```

use jarvis_avatar::a2f::{A2fClient, A2fConfig};
use jarvis_avatar::kokoro_http::{fetch_kokoro_speech, kokoro_pcm_bytes_to_a2f_input};

fn env_req(key: &str) -> Option<String> {
    let v = std::env::var(key).ok()?;
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

#[tokio::test]
#[ignore = "requires live Kokoro + Audio2Face; set JARVIS_KOKORO_URL and JARVIS_A2F_ENDPOINT"]
async fn kokoro_wav_then_a2f_returns_keyframes() {
    let kokoro = env_req("JARVIS_KOKORO_URL").expect("JARVIS_KOKORO_URL");
    let a2f_ep = env_req("JARVIS_A2F_ENDPOINT").expect("JARVIS_A2F_ENDPOINT");
    let health = env_req("JARVIS_A2F_HEALTH_URL").expect("JARVIS_A2F_HEALTH_URL");
    let voice = env_req("JARVIS_TTS_VOICE").unwrap_or_else(|| "af_heart".into());

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .expect("client");

    let raw = fetch_kokoro_speech(
        &http,
        &kokoro,
        &voice,
        "Hello from the Jarvis A two F pipeline test.",
        "pcm",
        false,
    )
    .await
    .expect("kokoro pcm");
    let (pcm, rate) = kokoro_pcm_bytes_to_a2f_input(&raw, 24_000).expect("pcm16 mono");

    let client = A2fClient::new(A2fConfig {
        enabled: true,
        endpoint: a2f_ep,
        health_url: health,
        function_id: env_req("JARVIS_A2F_FUNCTION_ID")
            .unwrap_or_else(|| "0961a6da-fb9e-4f2e-8491-247e5fd7bf8d".into()),
    });
    let health = client.health().await;
    assert!(health.ok, "A2F HTTP health not ready: {:?}", health.error);

    let mut emotions = std::collections::HashMap::new();
    emotions.insert("joy".into(), 0.5);
    let out = client
        .process_audio_pcm16(pcm, rate, Some(emotions))
        .await
        .expect("a2f process");

    eprintln!(
        "A2F: {} blendshape names, {} keyframes @ {} Hz",
        out.blend_shape_names.len(),
        out.keyframes.len(),
        rate
    );
    assert!(
        !out.blend_shape_names.is_empty() || !out.keyframes.is_empty(),
        "expected some blendshape output from A2F"
    );
}
