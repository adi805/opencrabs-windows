//! #2107: the built-in provider serves transcription on the chat route, with
//! an `input_audio` part, and answers `404 page not found` on both
//! `/v1/audio/*` paths. The client only ever built the multipart request, so an
//! install that set `stt_model` on the built-in provider paid one failed round
//! trip per voice note and never got the free transcription.
//!
//! Everything pinned here is pure: route selection, format sniffing, the request
//! body and the reply parse. No network, no home directory.

use crate::brain::provider::opencrabs_install::OPENCRABS_BASE_URL;
use crate::channels::voice::openai_stt::{
    SttRoute, chat_audio_body, chat_audio_transcript, route_for, sniff_audio_format,
};

#[test]
fn the_builtin_provider_is_routed_to_the_chat_route() {
    // The STT candidate is the host root: the audio clients join `v1/...`
    // themselves, so the path must not decide the route.
    assert_eq!(
        route_for("https://inference.opencrabs.com"),
        SttRoute::ChatAudio
    );
    assert_eq!(route_for(OPENCRABS_BASE_URL), SttRoute::ChatAudio);
}

#[test]
fn every_other_engine_keeps_the_multipart_route() {
    for base in [
        "https://api.groq.com/openai/v1",
        "https://api.openai.com/v1",
        "http://localhost:11434/v1",
        "http://127.0.0.1:8080",
    ] {
        assert_eq!(
            route_for(base),
            SttRoute::Multipart,
            "{base} is an [OI]-compatible engine and answers /v1/audio/transcriptions"
        );
    }
}

#[test]
fn an_unparseable_base_url_stays_on_the_multipart_route() {
    assert_eq!(route_for("not a url"), SttRoute::Multipart);
    assert_eq!(route_for(""), SttRoute::Multipart);
}

#[test]
fn the_audio_container_is_read_from_the_magic_bytes() {
    assert_eq!(sniff_audio_format(b"OggS\x00\x02"), "ogg");
    assert_eq!(sniff_audio_format(b"RIFF\x24\x08\x00\x00WAVEfmt "), "wav");
    assert_eq!(sniff_audio_format(b"ID3\x04\x00"), "mp3");
    assert_eq!(sniff_audio_format(b"fLaC\x00\x00"), "flac");
    assert_eq!(sniff_audio_format(b"\x00\x00\x00\x20ftypM4A "), "m4a");
    // Unknown bytes keep the Telegram default rather than failing the call.
    assert_eq!(sniff_audio_format(b"\x01\x02\x03"), "ogg");
    assert_eq!(sniff_audio_format(b""), "ogg");
}

#[test]
fn the_chat_body_carries_an_input_audio_part() {
    let body = chat_audio_body("whisper-1", "QUJD", "ogg");
    assert_eq!(body["model"], "whisper-1");
    let content = body["messages"][0]["content"]
        .as_array()
        .expect("content must be an array of parts");
    assert!(
        content.iter().any(|p| p["type"] == "input_audio"
            && p["input_audio"]["data"] == "QUJD"
            && p["input_audio"]["format"] == "ogg"),
        "the request must carry the audio as an input_audio part: {body}"
    );
    assert!(
        content.iter().any(|p| p["type"] == "text"),
        "the part needs an instruction next to the audio: {body}"
    );
}

#[test]
fn the_transcript_is_read_out_of_the_reply() {
    let reply = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": "  halo dunia  "}}]
    });
    assert_eq!(chat_audio_transcript(&reply).as_deref(), Some("halo dunia"));
}

#[test]
fn an_empty_or_shapeless_reply_is_not_a_transcript() {
    for reply in [
        serde_json::json!({"choices": [{"message": {"content": "   "}}]}),
        serde_json::json!({"choices": []}),
        serde_json::json!({"error": "nope"}),
        serde_json::json!(null),
    ] {
        assert_eq!(
            chat_audio_transcript(&reply),
            None,
            "no text must be reported as no text, not as an empty transcript: {reply}"
        );
    }
}
