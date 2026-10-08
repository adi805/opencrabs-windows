//! Live location (audit §10.5): `edit_message_live_location` and
//! `stop_message_live_location` both EDIT an existing message instead of
//! creating one, so they resolve through `resolve_existing_target` (chat plus
//! a required `message_id`), and never through the new-message seam whose cron
//! allowlist exists to stop a fresh send landing in an unconfigured chat.
//!
//! Neither payload carries a `message_thread_id` setter at all: a `message_id`
//! is unique within its chat, so the topic is implied by the message being
//! edited. That is why these two are addressed by `message_id` rather than
//! `thread_id` like the `send_*` family.
//!
//! The resolver's own refusal is already pinned in
//! `telegram_target_resolver_test`, so nothing here re-tests it: these tests
//! cover only what is specific to the two new actions.

use crate::brain::tools::telegram_send::TelegramSendTool;
use crate::brain::tools::r#trait::Tool;
use crate::channels::telegram::TelegramState;
use std::sync::Arc;

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

#[test]
fn the_action_description_names_both_live_location_actions() {
    // The enum decides what dispatch accepts; the description is what tells
    // the model the pair exists at all. An action that is dispatchable but
    // never described is one the model does not reach for, which reads as the
    // capability being absent even though the arm is there.
    let tool = TelegramSendTool::new(Arc::new(TelegramState::new()));
    let schema = tool.input_schema();
    let description = schema["properties"]["action"]["description"]
        .as_str()
        .expect("the action property carries a description");
    for action in ["edit_message_live_location", "stop_message_live_location"] {
        assert!(
            description.contains(action),
            "{action} is dispatchable but the action description never names it"
        );
    }
}
