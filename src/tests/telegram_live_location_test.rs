//! Live location (audit §10.5): `edit_message_live_location` and
//! `stop_message_live_location` both EDIT an existing message instead of
//! creating one, so they resolve through `resolve_existing_target` (chat plus
//! a required `message_id`), and never through the new-message seam whose cron
//! allowlist exists to stop a fresh send landing in an unconfigured chat.
//!
//! Neither payload carries a `message_thread_id` setter at all: a `message_id`
//! is unique within its chat, so the topic is implied by the message being
//! edited. That is why these two are pinned against `message_id` here rather
//! than `thread_id` like the `send_*` family.

use crate::brain::tools::r#trait::Tool;
use crate::brain::tools::telegram_send::{TelegramSendTool, resolve_existing_target};
use crate::channels::telegram::TelegramState;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

fn empty_state() -> TelegramState {
    TelegramState::new()
}

fn action_enum(tool: &TelegramSendTool) -> Vec<String> {
    tool.input_schema()["properties"]["action"]["enum"]
        .as_array()
        .expect("action enum is an array")
        .iter()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect()
}

#[test]
fn the_schema_offers_both_live_location_actions() {
    // A schema entry with no match arm is an action the model will call and
    // dispatch will reject as unknown, which reads to the user as the agent
    // inventing a capability. `every_telegram_action_is_reachable` in
    // `readme_channel_surface_test` checks the arm; this checks the entry.
    let tool = TelegramSendTool::new(Arc::new(TelegramState::new()));
    let actions = action_enum(&tool);
    for action in ["edit_message_live_location", "stop_message_live_location"] {
        assert!(
            actions.iter().any(|a| a == action),
            "{action} missing from the schema enum"
        );
    }
}

#[test]
fn the_schema_offers_the_live_location_tuning_parameters() {
    // `edit_message_live_location` can retune the live period and the
    // accuracy / heading / proximity fields. A parameter with no schema entry
    // is one the model cannot set, which reads as the capability not existing.
    let tool = TelegramSendTool::new(Arc::new(TelegramState::new()));
    let props = tool.input_schema()["properties"].clone();
    for param in [
        "live_period",
        "horizontal_accuracy",
        "heading",
        "proximity_alert_radius",
    ] {
        assert!(
            props.get(param).is_some(),
            "{param} missing from the schema properties"
        );
    }
}

#[tokio::test]
async fn a_live_location_edit_needs_a_message_id() {
    // Both actions edit an existing message, so the shared seam refuses a call
    // that names no target. Without this the tool would have to guess which
    // live location to move.
    let err = resolve_existing_target(&json!({}), Uuid::nil(), &empty_state())
        .await
        .expect_err("a call with no message_id must not resolve");
    let text = err.error.unwrap_or_default();
    assert!(
        text.contains("message_id"),
        "refusal should name the missing field: {text}"
    );
}
