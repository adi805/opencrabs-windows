//! `copy_message` (#100): copying a message into another chat is a deliberate
//! move, and like every other message-creating action it must clear the cron
//! allowlist before it can reach the wire.
//!
//! `CopyMessage` is the one Bot API method this tool wraps that answers with a
//! bare `MessageId` instead of a `Message`, so the action arm reads `sent.0`
//! where `forward` reads `m.id.0`. That shape difference is pinned here next
//! to the destination guard, because a copy that silently routed into an
//! unconfigured chat would be the same leak `cron_send_scope` exists to stop.

use crate::brain::tools::telegram_send::{TelegramSendTool, resolve_new_target};
use crate::brain::tools::r#trait::{Tool, ToolResult};
use crate::channels::telegram::TelegramState;
use crate::cron::send_scope::with_send_target;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

/// A chat a job was configured to post to, and one it was not.
const CONFIGURED: i64 = -1004252074515;
const SOMEWHERE_ELSE: i64 = -1004428873948;

// Fresh state + nil session: no session-origin chat/topic bound, no owner
// chat, no global DB pool — so the only chat source is the explicit input.
fn empty_state() -> TelegramState {
    TelegramState::new()
}

fn error_text(r: ToolResult) -> String {
    assert!(!r.success, "expected an error ToolResult");
    r.error.unwrap_or_default()
}

#[test]
fn the_schema_offers_copy_message() {
    // A schema entry with no match arm is an action the model will call and
    // dispatch will reject as unknown, which reads to the user as the agent
    // inventing a capability. `every_telegram_action_is_reachable` in
    // `readme_channel_surface_test` checks the arm; this checks the entry.
    let tool = TelegramSendTool::new(Arc::new(TelegramState::new()));
    let schema = tool.input_schema();
    let enum_strs: Vec<&str> = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action enum is an array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        enum_strs.contains(&"copy_message"),
        "copy_message missing from the schema enum"
    );
}

#[tokio::test]
async fn copy_message_to_the_configured_chat_resolves() {
    with_send_target(Some(CONFIGURED), async {
        let input = json!({ "chat_id": CONFIGURED, "from_chat_id": 5, "message_id": 77 });
        let target = resolve_new_target(&input, Uuid::nil(), &empty_state())
            .await
            .expect("the configured chat is in scope");
        assert_eq!(target.chat_id, CONFIGURED);
    })
    .await;
}

#[tokio::test]
async fn copy_message_outside_the_allowlist_is_refused() {
    // The leak this guards: a chat id recalled from memory or earlier context
    // is not permission to copy into it. The refusal lives on the shared
    // resolution seam, so it is not copy-specific — it is pinned here because
    // #100 is the action that made that seam reachable for a copy.
    with_send_target(Some(CONFIGURED), async {
        let input = json!({ "chat_id": SOMEWHERE_ELSE, "from_chat_id": 5, "message_id": 77 });
        let err = resolve_new_target(&input, Uuid::nil(), &empty_state())
            .await
            .expect_err("a chat outside the allowlist must not resolve");
        let text = error_text(err);
        assert!(
            text.contains("deliver_to"),
            "refusal should name what to change: {text}"
        );
        assert!(
            text.contains(&CONFIGURED.to_string()),
            "refusal should name the configured chat: {text}"
        );
    })
    .await;
}

#[tokio::test]
async fn copy_message_outside_a_job_is_unrestricted() {
    // The allowlist exists to stop a scheduled job reaching chats it was never
    // given, not to police an ordinary interactive copy.
    let input = json!({ "chat_id": SOMEWHERE_ELSE, "from_chat_id": 5, "message_id": 77 });
    let target = resolve_new_target(&input, Uuid::nil(), &empty_state())
        .await
        .expect("outside a job any explicit chat resolves");
    assert_eq!(target.chat_id, SOMEWHERE_ELSE);
}
