//! Regression tests for #148: the `http_request` tool used to buffer the whole
//! response body with `.text()` and then pretty-print JSON without applying the
//! output limit that plain text got. These tests pin the bounded read and the
//! single output budget that now covers JSON and text alike.

use crate::brain::tools::http::HttpClientTool;
use crate::brain::tools::{Tool, ToolExecutionContext};
use serde_json::json;
use uuid::Uuid;

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext::new(Uuid::new_v4()).with_auto_approve(true)
}

#[tokio::test]
async fn a_large_json_body_is_truncated_by_the_output_budget() {
    let mut server = mockito::Server::new_async().await;
    let url = server.url();

    // A JSON string value big enough that the rendered form is well past the
    // 10_000-char output budget. Before #148 this path skipped the limit.
    let big = "x".repeat(30_000);
    let body = json!({ "blob": big }).to_string();

    let _mock = server
        .mock("GET", "/big-json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create_async()
        .await;

    let tool = HttpClientTool;
    let input = json!({ "method": "GET", "url": format!("{}/big-json", url) });
    let result = tool.execute(input, &ctx()).await.expect("tool execute");

    assert!(result.success);
    assert!(
        result.output.contains("truncated to"),
        "a large JSON body must hit the shared output budget"
    );
    assert!(
        result.output.len() < 20_000,
        "the rendered body must not be dumped in full (got {} bytes)",
        result.output.len()
    );
}

#[tokio::test]
async fn a_large_text_body_is_truncated_by_the_output_budget() {
    let mut server = mockito::Server::new_async().await;
    let url = server.url();

    // Not valid JSON, so it takes the text path. It must obey the same budget.
    let body = "a".repeat(12_000);

    let _mock = server
        .mock("GET", "/big-text")
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;

    let tool = HttpClientTool;
    let input = json!({ "method": "GET", "url": format!("{}/big-text", url) });
    let result = tool.execute(input, &ctx()).await.expect("tool execute");

    assert!(result.success);
    assert!(
        result.output.contains("truncated to"),
        "a large text body must hit the shared output budget"
    );
}

#[tokio::test]
async fn a_small_body_passes_through_intact() {
    let mut server = mockito::Server::new_async().await;
    let url = server.url();

    let _mock = server
        .mock("GET", "/small")
        .with_status(200)
        .with_body(r#"{"ok":true}"#)
        .create_async()
        .await;

    let tool = HttpClientTool;
    let input = json!({ "method": "GET", "url": format!("{}/small", url) });
    let result = tool.execute(input, &ctx()).await.expect("tool execute");

    assert!(result.success);
    assert!(
        result.output.contains("\"ok\": true"),
        "small JSON is pretty-printed intact: {}",
        result.output
    );
    assert!(
        !result.output.contains("truncated"),
        "a small body must not be marked truncated"
    );
}

#[tokio::test]
async fn a_body_over_the_read_ceiling_is_cut_and_says_so() {
    let mut server = mockito::Server::new_async().await;
    let url = server.url();

    // Just past the 10 MiB read ceiling, as plain text so the byte count maps
    // directly onto the ceiling. The point is that the read stops there instead
    // of buffering the whole thing.
    let body = "z".repeat(10 * 1024 * 1024 + 4096);

    let _mock = server
        .mock("GET", "/huge")
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;

    let tool = HttpClientTool;
    let input = json!({ "method": "GET", "url": format!("{}/huge", url) });
    let result = tool.execute(input, &ctx()).await.expect("tool execute");

    assert!(result.success);
    assert!(
        result.output.contains("read ceiling"),
        "a body past the read ceiling must report the cut"
    );
}
