//! Shared Kokoro FastAPI HTTP helpers (speech fetch + PCM16 for A2F / tests).
//!
//! Kokoro’s OpenAI-compatible schema defaults **`stream: true`**, which can yield
//! concatenated chunks and **ill-formed WAV** for non-streaming clients. We default
//! **`stream: false`** for reliable single-shot bodies.
//!
//! **`response_format: "pcm"`** returns raw little-endian s16 mono (no WAV header);
//! Kokoro uses **24000 Hz** for that path (see upstream `OpenAISpeechRequest` / audio service).

use std::io::Cursor;
use std::time::Duration;

use hound::{SampleFormat, WavReader, WavSpec};
use reqwest::Client;

/// POST `/v1/audio/speech` with explicit format and stream flag.
///
/// `response_format`: `wav`, `pcm`, `mp3`, `opus`, `flac`, … (Kokoro-supported).
/// Use **`stream: false`** unless you implement full streaming assembly.
pub async fn fetch_kokoro_speech(
    client: &Client,
    base_url: &str,
    voice: &str,
    text: &str,
    response_format: &str,
    stream: bool,
) -> Result<Vec<u8>, String> {
    let endpoint = format!("{}/v1/audio/speech", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": "kokoro",
        "voice": voice,
        "input": text,
        "response_format": response_format,
        "stream": stream,
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
        return Err(format!(
            "kokoro {status}: {}",
            txt.chars().take(200).collect::<String>()
        ));
    }

    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("read bytes: {e}"))
}

/// Shorthand: WAV, non-streaming (Bevy / rodio path).
pub async fn fetch_kokoro_wav(
    client: &Client,
    base_url: &str,
    voice: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    fetch_kokoro_speech(client, base_url, voice, text, "wav", false).await
}

/// Raw PCM s16le mono bytes for A2F (`sample_rate` is typically **24000** for Kokoro PCM).
pub async fn fetch_kokoro_pcm_s16le(
    client: &Client,
    base_url: &str,
    voice: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    fetch_kokoro_speech(client, base_url, voice, text, "pcm", false).await
}

/// Drop a stray odd byte if the server returns an uneven buffer.
pub fn trim_pcm_s16le_mono(pcm: &[u8]) -> Vec<u8> {
    let n = pcm.len() & !1;
    pcm[..n].to_vec()
}

/// Wrap Kokoro PCM (s16le mono) in a WAV container for `bevy_audio` / `hound` validation.
pub fn pcm_s16le_mono_to_wav_bytes(pcm: &[u8], sample_rate: u32) -> Result<Vec<u8>, String> {
    let pcm = trim_pcm_s16le_mono(pcm);
    if pcm.is_empty() {
        return Err("empty PCM".into());
    }
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut out: Vec<u8> = Vec::new();
    {
        let cur = Cursor::new(&mut out);
        let mut w = hound::WavWriter::new(cur, spec).map_err(|e| format!("wav writer: {e}"))?;
        for chunk in pcm.chunks_exact(2) {
            let v = i16::from_le_bytes([chunk[0], chunk[1]]);
            w.write_sample(v)
                .map_err(|e| format!("wav write sample: {e}"))?;
        }
        w.finalize().map_err(|e| format!("wav finalize: {e}"))?;
    }
    Ok(out)
}

/// Decode WAV to **little-endian PCM16 mono** interleaved bytes + sample rate for A2F.
pub fn wav_bytes_to_pcm16_mono(wav: &[u8]) -> Result<(Vec<u8>, u32), String> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(format!(
            "not a WAV (first bytes: {:02x?})",
            &wav[..wav.len().min(12)]
        ));
    }

    let mut reader = WavReader::new(Cursor::new(wav)).map_err(|e| format!("hound: {e}"))?;
    let spec = reader.spec();
    let rate = spec.sample_rate;

    let mut pcm: Vec<u8> = Vec::new();

    match (spec.sample_format, spec.bits_per_sample, spec.channels) {
        (SampleFormat::Int, 16, 1) => {
            for s in reader.samples::<i16>() {
                let v = s.map_err(|e| format!("sample: {e}"))?;
                pcm.extend_from_slice(&v.to_le_bytes());
            }
        }
        (SampleFormat::Int, 16, 2) => {
            let mut samples = reader.samples::<i16>();
            loop {
                let l = match samples.next() {
                    None => break,
                    Some(s) => s.map_err(|e| format!("sample: {e}"))?,
                };
                let r = match samples.next() {
                    None => break,
                    Some(s) => s.map_err(|e| format!("sample: {e}"))?,
                };
                let m = ((l as i32 + r as i32) / 2) as i16;
                pcm.extend_from_slice(&m.to_le_bytes());
            }
        }
        (SampleFormat::Float, 32, 1) => {
            for s in reader.samples::<f32>() {
                let v = s.map_err(|e| format!("sample: {e}"))?;
                let i = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
                pcm.extend_from_slice(&i.to_le_bytes());
            }
        }
        (SampleFormat::Float, 32, 2) => {
            let mut samples = reader.samples::<f32>();
            loop {
                let l = match samples.next() {
                    None => break,
                    Some(s) => s.map_err(|e| format!("sample: {e}"))?,
                };
                let r = match samples.next() {
                    None => break,
                    Some(s) => s.map_err(|e| format!("sample: {e}"))?,
                };
                let m = ((l + r) * 0.5).clamp(-1.0, 1.0);
                let i = (m * 32767.0) as i16;
                pcm.extend_from_slice(&i.to_le_bytes());
            }
        }
        (fmt, bits, ch) => {
            return Err(format!(
                "unsupported WAV for A2F: format={fmt:?} bits={bits} channels={ch} (need 16-bit int or 32-bit float, 1–2 ch)"
            ));
        }
    }

    if pcm.is_empty() {
        return Err("empty PCM after WAV decode".into());
    }

    Ok((pcm, rate))
}

/// Kokoro PCM → `(pcm16, sample_rate)` for A2F (no WAV round-trip).
pub fn kokoro_pcm_bytes_to_a2f_input(
    pcm: &[u8],
    sample_rate: u32,
) -> Result<(Vec<u8>, u32), String> {
    let pcm = trim_pcm_s16le_mono(pcm);
    if pcm.is_empty() {
        return Err("empty Kokoro PCM payload".into());
    }
    Ok((pcm, sample_rate))
}

/// Default HTTP client for Kokoro (matches in-app TTS thread timeout).
pub fn default_client() -> Result<Client, reqwest::Error> {
    Client::builder().timeout(Duration::from_secs(60)).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_to_wav_roundtrip_sample() {
        let mut pcm = Vec::new();
        for _ in 0..80 {
            pcm.extend_from_slice(&0i16.to_le_bytes());
        }
        let wav = pcm_s16le_mono_to_wav_bytes(&pcm, 24000).expect("wrap");
        let (back, rate) = wav_bytes_to_pcm16_mono(&wav).expect("unwrap");
        assert_eq!(rate, 24000);
        assert_eq!(back.len(), pcm.len());
    }
}
