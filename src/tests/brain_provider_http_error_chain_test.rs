//! `ProviderError::HttpError` must name the deepest transport cause.
//!
//! The retry log printed only reqwest's top-level text ("error sending
//! request for url (...)"), so a DNS or connect failure was invisible in the
//! log while the TUI showed the real reason. The Display now carries the
//! source chain. A closed local port gives a deterministic connect failure
//! with no network dependence.

use crate::brain::provider::ProviderError;

async fn closed_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let port = l.local_addr().expect("local addr").port();
    drop(l);
    port
}

#[tokio::test]
async fn http_error_display_names_the_connect_cause() {
    let port = closed_port().await;
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let err = reqwest::Client::new()
        .post(&url)
        .send()
        .await
        .expect_err("nothing is listening: the send must fail");
    let top_level = err.to_string();

    let shown = ProviderError::from(err).to_string();

    assert!(
        shown.starts_with("HTTP request failed: "),
        "keeps the prefix, got: {shown}"
    );
    assert!(
        shown.len() > top_level.len(),
        "must carry more than reqwest's top level, got: {shown}"
    );
    assert!(
        shown.to_ascii_lowercase().contains("refused"),
        "must name the refused connect, got: {shown}"
    );
}
