//! Post-bind transport failure classification (#1959).
//!
//! Sixteen A2A `transport_error` journal rows all carried the same opaque
//! `error sending request for url (...)` string, so the candidate mechanisms
//! (listener down vs accept stall vs mid-exchange drop) could not be
//! discriminated. These tests hit the REAL retry leg (`post_jsonrpc`) with
//! in-process listeners and pin the classification that replaces the
//! uniformity: a dead port must read `connect:`, a 502 without a JSON-RPC
//! body must name the response failure, and the failing attempt number must
//! be on the line.

use crate::cli::session_notify::post_jsonrpc;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// An ephemeral port that was bound and released: nothing is listening, so
/// a connect attempt is refused immediately (no firewall guesswork).
async fn closed_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let port = l.local_addr().expect("local addr").port();
    drop(l);
    port
}

#[tokio::test]
async fn dead_listener_classifies_as_connect() {
    let port = closed_port().await;
    let url = format!("http://127.0.0.1:{port}/a2a/v1");
    let err = post_jsonrpc(
        &url,
        None,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "x"}),
    )
    .await
    .expect_err("nothing is listening: the send must fail");
    assert!(
        err.contains("connect:"),
        "must name the connect phase, got: {err}"
    );
    assert!(
        !err.contains("error sending request for url"),
        "the bare opaque Display must no longer be the whole detail: {err}"
    );
    assert!(
        err.contains("(attempt 3/3)"),
        "the journal must show the LAST attempt failed, got: {err}"
    );
}

#[tokio::test]
async fn http_502_without_jsonrpc_names_the_response_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind canned server");
    let port = listener.local_addr().expect("local addr").port();
    let server = tokio::spawn(async move {
        // One 502 per arriving connection (the leg retries three times).
        for _ in 0..4 {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
        }
    });

    let url = format!("http://127.0.0.1:{port}/a2a/v1");
    let err = post_jsonrpc(
        &url,
        None,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "x"}),
    )
    .await
    .expect_err("a 502 with no JSON-RPC body carries no decision: must fail");
    assert!(
        err.contains("without a JSON-RPC body"),
        "must name the response-shape failure, got: {err}"
    );
    assert!(
        err.contains("502"),
        "must carry the status code, got: {err}"
    );
    assert!(
        err.contains("(attempt 3/3)"),
        "the journal must show the LAST attempt failed, got: {err}"
    );
    server.abort();
}
