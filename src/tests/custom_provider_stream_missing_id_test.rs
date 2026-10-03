//! Regression test: InferHub's final usage-only SSE chunk omits the `id`
//! field (and sometimes `choices`). Before the fix, serde rejected the whole
//! chunk with "missing field `id`", so the provider's real token counts were
//! dropped and the ledger fell back to a gross local estimate with
//! `output_tokens = 0`. `OpenAIStreamChunk` now defaults both fields, and the
//! existing `!chunk.id.is_empty()` / `chunk.choices.is_empty()` guards already
//! handle the empty defaults.

use crate::brain::provider::OpenAIProvider;
use crate::brain::provider::Provider;
use crate::brain::provider::{ContentDelta, LLMRequest, Message, StreamEvent};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

fn chunk_role(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","object":"chat.completion.chunk","model":"m","choices":[{{"index":0,"delta":{{"role":"assistant"}},"finish_reason":null}}]}}"#
    )
}

fn chunk_text_finish(id: &str, text: &str) -> String {
    format!(
        r#"{{"id":"{id}","object":"chat.completion.chunk","model":"m","choices":[{{"index":0,"delta":{{"content":"{text}"}},"finish_reason":"stop"}}]}}"#
    )
}

/// The exact production shape: usage-only chunk with NO `id`, `choices: []`.
fn chunk_usage_no_id(usage: &str) -> String {
    format!(r#"{{"object":"chat.completion.chunk","model":"m","choices":[],"usage":{usage}}}"#)
}

/// Stricter shape: no `id` AND no `choices` field at all.
fn chunk_usage_no_id_no_choices(usage: &str) -> String {
    format!(r#"{{"object":"chat.completion.chunk","model":"m","usage":{usage}}}"#)
}

async fn serve_sse(listener: TcpListener, body: String) {
    let (mut sock, _) = listener.accept().await.expect("accept");
    let mut buf = [0u8; 8192];
    let _ = timeout(Duration::from_secs(5), sock.read(&mut buf)).await;
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    sock.write_all(resp.as_bytes()).await.expect("write sse");
    sock.flush().await.ok();
}

async fn collect_events(provider: &OpenAIProvider) -> Vec<StreamEvent> {
    let req = LLMRequest::new("test-model", vec![Message::user("ping")]);
    let mut stream = provider.stream(req).await.expect("stream opens");
    let mut events = Vec::new();
    while let Some(ev) = futures::StreamExt::next(&mut stream).await {
        let ev = ev.expect("event ok");
        let done = matches!(ev, StreamEvent::MessageStop);
        events.push(ev);
        if done {
            break;
        }
    }
    events
}

/// Sum of output tokens across every MessageDelta the stream produced.
/// Pre-fix the usage-only chunk is dropped, so only the `[DONE]` fallback
/// delta survives and this returns 0.
fn output_tokens(events: &[StreamEvent]) -> u32 {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::MessageDelta { usage, .. } => Some(usage.output_tokens),
            _ => None,
        })
        .sum()
}

#[tokio::test]
async fn usage_only_chunk_without_id_reaches_the_ledger() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    let id = "chatcmpl-inferhub";
    let sse = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunk_role(id),
        chunk_text_finish(id, "PONG"),
        chunk_usage_no_id(r#"{"prompt_tokens":11,"completion_tokens":22}"#),
    );
    tokio::spawn(serve_sse(listener, sse));

    let provider = OpenAIProvider::local(format!("http://127.0.0.1:{port}/chat/completions"));
    let events = timeout(Duration::from_secs(10), collect_events(&provider))
        .await
        .expect("stream completes in time");

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ContentBlockDelta {
                delta: ContentDelta::TextDelta { text },
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "PONG");
    assert_eq!(
        output_tokens(&events),
        22,
        "real usage must reach the ledger, not the 0 fallback"
    );
    assert!(matches!(events.last(), Some(StreamEvent::MessageStop)));
}

#[tokio::test]
async fn usage_only_chunk_without_id_or_choices_reaches_the_ledger() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    let id = "chatcmpl-inferhub2";
    let sse = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunk_role(id),
        chunk_text_finish(id, "PONG"),
        chunk_usage_no_id_no_choices(r#"{"prompt_tokens":5,"completion_tokens":7}"#),
    );
    tokio::spawn(serve_sse(listener, sse));

    let provider = OpenAIProvider::local(format!("http://127.0.0.1:{port}/chat/completions"));
    let events = timeout(Duration::from_secs(10), collect_events(&provider))
        .await
        .expect("stream completes in time");

    assert_eq!(
        output_tokens(&events),
        7,
        "real usage must reach the ledger even when choices is absent"
    );
}
