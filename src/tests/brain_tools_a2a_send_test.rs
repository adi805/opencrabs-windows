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
fn test_discover_does_not_require_approval() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "discover", "url": "http://localhost:18790"});
    assert!(!tool.requires_approval_for_input(&input));
}

#[test]
fn test_send_requires_approval() {
    let tool = A2aSendTool::new();
    let input =
        serde_json::json!({"action": "send", "url": "http://localhost:18790", "message": "hi"});
    assert!(tool.requires_approval_for_input(&input));
}

#[test]
fn test_cancel_requires_approval() {
    let tool = A2aSendTool::new();
    let input =
        serde_json::json!({"action": "cancel", "url": "http://localhost:18790", "task_id": "abc"});
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
    let input = serde_json::json!({"action": "send", "url": "http://localhost:18790"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    assert!(result.error.as_deref().unwrap_or("").contains("message"));
}

#[tokio::test]
async fn test_missing_task_id_for_get() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "get", "url": "http://localhost:18790"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    assert!(result.error.as_deref().unwrap_or("").contains("task_id"));
}

#[tokio::test]
async fn test_missing_task_id_for_cancel() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "cancel", "url": "http://localhost:18790"});
    let result = tool.execute(input, &ctx()).await.unwrap();
    assert!(!result.success);
    assert!(result.error.as_deref().unwrap_or("").contains("task_id"));
}

#[tokio::test]
async fn test_unknown_action() {
    let tool = A2aSendTool::new();
    let input = serde_json::json!({"action": "invalid", "url": "http://localhost:18790"});
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

// ─── COR-02: cancel must not report a false success ────────────────────────

#[test]
fn test_cancel_outcome_accepts_cancelled_states() {
    // Both spellings the A2A spec and its implementations emit count as a real
    // cancellation, matched case-insensitively.
    for state in ["cancelled", "canceled", "CANCELLED", "Canceled"] {
        let result = cancel_outcome("t1", state);
        assert!(
            result.success,
            "state {state:?} should be a successful cancellation: {:?}",
            result.error
        );
        assert!(result.output.contains("cancelled"), "{}", result.output);
    }
}

#[test]
fn test_cancel_outcome_rejects_terminal_states() {
    // COR-02: the remote agent answers `tasks/cancel` with whatever state the
    // task is already in. Reporting "cancelled" for `completed` is the false
    // success this helper exists to prevent.
    for state in ["completed", "failed", "working", "unknown", ""] {
        let result = cancel_outcome("t1", state);
        assert!(
            !result.success,
            "state {state:?} must NOT be reported as cancelled"
        );
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("was NOT cancelled"),
            "error must say it was not cancelled: {err}"
        );
        assert!(
            err.contains(state),
            "error must echo the state the server sent ({state:?}): {err}"
        );
    }
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
        "url": "http://localhost:18790",
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
