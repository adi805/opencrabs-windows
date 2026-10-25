//! STT clients for [OI]-compatible providers.
//!
//! Two request shapes exist, and the base URL decides which one is used
//! (`route_for`):
//!
//! - **Multipart**, `POST /v1/audio/transcriptions`. The [OI] API, Groq
//!   (`https://api.groq.com/openai/v1`), Ollama (`http://localhost:11434/v1`),
//!   LocalAI, OMLX, and every other engine that implements the audio routes.
//! - **Chat audio**, `POST /v1/chat/completions` carrying an `input_audio`
//!   part. This is the built-in provider, which answers 404 on every
//!   `/v1/audio/*` path.
//!
//! Both return the same thing: a transcript string.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;

use super::openai_tts::build_endpoint_url;

/// Transcribe audio bytes using an OpenAI-compatible STT endpoint.
///
/// # Arguments
/// * `audio_bytes` - OGG/Opus audio data
/// * `api_key` - API key for the endpoint
/// * `model` - Model name (e.g. "whisper-large-v3-turbo")
/// * `base_url` - Base URL of the endpoint
///
/// # Returns
/// Transcribed text.
pub async fn transcribe_audio(
    audio_bytes: Vec<u8>,
    api_key: &str,
    model: &str,
    base_url: &str,
) -> Result<String> {
    let transcribe_url = build_endpoint_url(base_url, "v1/audio/transcriptions")?;

    let client = Client::new();

    let model_owned = model.to_string();

    let file_part = reqwest::multipart::Part::bytes(audio_bytes)
        .file_name("voice.ogg")
        .mime_str("audio/ogg")?;

    let form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", model_owned)
        .text("response_format", "json");

    let response = client
        .post(&transcribe_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await
        .context("Failed to send audio to STT endpoint")?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_default();
        anyhow::bail!("STT error ({}): {}", status, error_text);
    }

    let result: TranscriptionResponse = response
        .json()
        .await
        .context("Failed to parse transcription response")?;

    tracing::info!(
        "OpenAI-compatible STT: transcribed {} chars (model={}, url={})",
        result.text.len(),
        model,
        transcribe_url,
    );

    Ok(result.text)
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

// ---------------------------------------------------------------------------
// #2107: the built-in provider serves transcription on the CHAT route, with an
// `input_audio` part, and answers 404 on `/v1/audio/transcriptions`. The client
// only ever built the multipart request, so a provider that advertised an
// `stt_model` cost one failed round trip per voice note and the free
// transcription never arrived.
// ---------------------------------------------------------------------------

/// Which request shape a provider answers for speech-to-text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttRoute {
    /// `POST /v1/audio/transcriptions`, multipart. Every [OI]-compatible
    /// engine: Groq, LocalAI, OMLX, Ollama.
    Multipart,
    /// `POST /v1/chat/completions` carrying an `input_audio` part. The
    /// built-in OpenCrabs provider, which answers no `/v1/audio/*` route.
    ChatAudio,
}

/// Pick the route from the configured base URL.
///
/// The built-in provider is identified by HOST, not by path: its STT candidate
/// is reduced to the host root because the audio clients join `v1/...` onto it
/// themselves (`factory::audio_candidate`), so a path comparison would miss it.
/// A URL that does not parse stays on the multipart route, which is what every
/// other engine expects.
pub fn route_for(base_url: &str) -> SttRoute {
    let host = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    let builtin_host =
        reqwest::Url::parse(crate::brain::provider::opencrabs_install::OPENCRABS_BASE_URL)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
    match (host, builtin_host) {
        (Some(a), Some(b)) if a.eq_ignore_ascii_case(&b) => SttRoute::ChatAudio,
        _ => SttRoute::Multipart,
    }
}

/// Container format of the audio bytes, from the leading magic bytes.
///
/// Telegram voice notes are OGG/Opus; a forwarded file can be anything. The
/// chat route needs the format named, so guess it from the bytes rather than
/// from a caller that only ever passes raw audio.
pub fn sniff_audio_format(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"OggS") {
        "ogg"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        "wav"
    } else if bytes.starts_with(b"ID3") || bytes.starts_with(&[0xFF, 0xFB]) {
        "mp3"
    } else if bytes.starts_with(b"fLaC") {
        "flac"
    } else if bytes.get(4..8) == Some(b"ftyp") {
        "m4a"
    } else {
        "ogg"
    }
}

/// Request body for the chat route. Pure, so the shape is testable without a
/// network call.
pub fn chat_audio_body(model: &str, audio_b64: &str, format: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "Transcribe this audio verbatim. Reply with the transcript only."},
                {"type": "input_audio", "input_audio": {"data": audio_b64, "format": format}}
            ]
        }],
        "temperature": 0
    })
}

/// The transcript out of a chat-completions response.
pub fn chat_audio_transcript(body: &serde_json::Value) -> Option<String> {
    let content = &body["choices"].as_array()?.first()?["message"]["content"];
    let text = content.as_str()?.trim();
    (!text.is_empty()).then_some(text.to_string())
}

/// Transcribe through the chat route: base64 the audio, post it as an
/// `input_audio` part, read the reply text.
pub async fn transcribe_audio_chat(
    audio_bytes: Vec<u8>,
    api_key: &str,
    model: &str,
    base_url: &str,
) -> Result<String> {
    use base64::Engine as _;

    let url = build_endpoint_url(base_url, "v1/chat/completions")?;
    let format = sniff_audio_format(&audio_bytes);
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&audio_bytes);
    let body = chat_audio_body(model, &audio_b64, format);

    let response = Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await
        .context("Failed to send audio to the chat transcription route")?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("STT error ({}): {}", status, text);
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&text).context("Failed to parse chat transcription response")?;
    let transcript = chat_audio_transcript(&parsed)
        .ok_or_else(|| anyhow::anyhow!("Chat transcription reply carried no text: {text}"))?;

    tracing::info!(
        "Chat-route STT: transcribed {} chars (model={}, url={})",
        transcript.len(),
        model,
        url,
    );
    Ok(transcript)
}
