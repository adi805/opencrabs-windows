use crate::brain::tools::a2a_send::*;
use crate::brain::tools::{Tool, ToolExecutionContext};
use tokio;

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext::new(uuid::Uuid::new_v4())
}

#[test]
fn test_tool_name_and_schema() {
    let tool = A2aSendTool::new();
    assert_eq!(tool.name(), "a2a_send");
    let schema = tool.input_schema();
    let props = schema["properties"].as_object().unwrap();
    assert!(props.contains_key("action"));
    assert!(props.contains_key("url"));
    assert!(props.contains_key("message"));
    assert!(props.contains_key("task_id"));
    assert!(props.contains_key("api_key"));
}

#[test]
fn test_discover_requires_approval() {
    // OC-A1: discovery returns a response body to the model from a
    // model-supplied URL, so it is gated like every other action.
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "discover", "url": "http://127.0.0.1:18790"});
    assert!(tool.requires_approval_for_input(&input));
}

#[test]
fn test_send_requires_approval() {
    let tool = A2aSendTool::new();
    let input =
        serde_json::json!({"action": "send", "url": "http://127.0.0.1:18790", "message": "hi"});
    assert!(tool.requires_approval_for_input(&input));
}

#[test]
fn test_cancel_requires_approval() {
    let tool = A2aSendTool::new();
    let input =
        serde_json::json!({"action": "cancel", "url": "http://127.0.0.1:18790", "task_id": "abc"});
    assert!(tool.requires_approval_for_input(&input));
}

#[tokio::test]
async fn test_missing_url() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "discover"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    assert!(result.error.as_deref().unwrap_or("").contains("url"));
}

#[tokio::test]
async fn test_missing_message_for_send() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "send", "url": "http://127.0.0.1:18790"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    assert!(result.error.as_deref().unwrap_or("").contains("message"));
}

#[tokio::test]
async fn test_missing_task_id_for_get() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "get", "url": "http://127.0.0.1:18790"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    assert!(result.error.as_deref().unwrap_or("").contains("task_id"));
}

#[tokio::test]
async fn test_missing_task_id_for_cancel() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "cancel", "url": "http://127.0.0.1:18790"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    assert!(result.error.as_deref().unwrap_or("").contains("task_id"));
}

#[tokio::test]
async fn test_unknown_action() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "invalid", "url": "http://127.0.0.1:18790"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Unknown action")
    );
}

#[test]
fn test_extract_response_text_from_status_message() {
    let task = serde_json::json!({
        "status": {
            "state": "completed",
            "message": {
                "role": "agent",
                "parts": [{"text": "Hello from agent"}]
            }
        }
    });
    assert_eq!(extract_response_text(&task), "Hello from agent");
}

#[test]
fn test_extract_response_text_from_artifacts() {
    let task = serde_json::json!({
        "status": {"state": "completed"},
        "artifacts": [{
            "parts": [{"text": "Result A"}, {"text": "Result B"}]
        }]
    });
    assert_eq!(extract_response_text(&task), "Result A\nResult B");
}

#[test]
fn test_extract_response_text_combined() {
    let task = serde_json::json!({
        "status": {
            "state": "completed",
            "message": {"role": "agent", "parts": [{"text": "Status msg"}]}
        },
        "artifacts": [{"parts": [{"text": "Artifact text"}]}]
    });
    assert_eq!(extract_response_text(&task), "Status msg\nArtifact text");
}

#[test]
fn test_extract_response_text_empty() {
    let task = serde_json::json!({"status": {"state": "working"}});
    assert_eq!(extract_response_text(&task), "");
}

#[test]
fn test_auth_headers_with_key() {
    let headers = auth_headers(Some("my-secret"));
    let auth = headers.get(reqwest::header::AUTHORIZATION).unwrap();
    assert_eq!(auth.to_str().unwrap(), "Bearer my-secret");
}

#[test]
fn test_auth_headers_without_key() {
    let headers = auth_headers(None);
    assert!(headers.get(reqwest::header::AUTHORIZATION).is_none());
}

#[test]
fn test_default_impl() {
    let tool = A2aSendTool;
    assert_eq!(tool.name(), "a2a_send");
}

#[tokio::test]
async fn test_url_and_profile_mutually_exclusive() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({
        "action": "discover",
        "url": "http://127.0.0.1:18790",
        "profile": "default"
    });
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    let err = result.error.as_deref().unwrap_or("");
    assert!(err.contains("mutually exclusive"), "got: {err}");
}

#[tokio::test]
async fn test_missing_target_names_both_options() {
    let tool = A2aSendTool::new();
    // No url, no profile -> error must teach the model both ways to target.
    let input = serde_json::json!({"action": "discover"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    let err = result.error.as_deref().unwrap_or("");
    assert!(
        err.contains("'url'") && err.contains("'profile'"),
        "got: {err}"
    );
}

/// OC-A1: the target is caller-supplied, so the shared SSRF guard runs before a
/// single byte goes out. A literal RFC1918 host is refused without a DNS
/// lookup, so this asserts the refusal, not a connect timeout.
#[tokio::test]
async fn test_rfc1918_target_is_refused_before_dialling() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "discover", "url": "http://10.0.0.5:18790"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    let err = result.error.as_deref().unwrap_or("");
    assert!(err.contains("a2a_send refused"), "got: {err}");
    assert!(err.contains("RFC1918"), "got: {err}");
}

#[tokio::test]
async fn test_metadata_target_is_refused_before_dialling() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "discover", "url": "http://169.254.169.254"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    let err = result.error.as_deref().unwrap_or("");
    assert!(err.contains("link-local"), "got: {err}");
}

/// The guard covers every action, not only `discover`.
#[tokio::test]
async fn test_rfc1918_target_is_refused_for_send_too() {
    let tool = A2aSendTool::new();
    let input =
        serde_json::json!({"action": "send", "url": "http://192.168.1.10:18790", "message": "hi"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("a2a_send refused")
    );
}

/// A wildcard bind is a *listen* address; the caller must still connect
/// somewhere, so it is rewritten to loopback instead of refused as unspecified.
#[test]
fn test_wildcard_bind_is_rewritten_to_loopback() {
    assert_eq!(
        connectable_base("http://0.0.0.0:18790"),
        "http://127.0.0.1:18790"
    );
    assert_eq!(connectable_base("http://[::]:18790"), "http://[::1]:18790");
    // Already-connectable and non-URL inputs pass through untouched.
    assert_eq!(
        connectable_base("http://127.0.0.1:18790"),
        "http://127.0.0.1:18790"
    );
    assert_eq!(
        connectable_base("https://peer.example"),
        "https://peer.example"
    );
}
